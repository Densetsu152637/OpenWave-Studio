//! Flush-to-zero / denormals-are-zero for the threads that run the audio graph.
//!
//! Denormal floats are slow on most CPUs and can make a decaying filter tail cost many times its
//! normal price. Every thread that processes nodes (the callback thread for the duration of a
//! cycle, every worker for its whole life) runs with FTZ and DAZ set, so results are also
//! bit-identical whichever thread runs a node.
//!
//! x86_64 sets MXCSR bits FTZ (15) and DAZ (6); aarch64 sets FPCR.FZ (24), which flushes
//! denormal inputs and outputs. Other targets are left unchanged. The register accesses are
//! not marked `nomem`/`readonly`, so the compiler treats each mode switch as a memory barrier
//! and cannot move floating-point work across it.

// Reading and writing the floating-point control register needs inline assembly.
#![allow(unsafe_code)]

/// Sets FTZ/DAZ on the current thread and restores the previous mode when dropped.
pub(crate) struct DenormalGuard {
    saved: arch::Word,
}

impl DenormalGuard {
    pub(crate) fn new() -> Self {
        let saved = arch::read();
        arch::write(saved | arch::FLAGS);
        Self { saved }
    }
}

impl Drop for DenormalGuard {
    fn drop(&mut self) {
        arch::write(self.saved);
    }
}

#[cfg(target_arch = "x86_64")]
mod arch {
    use std::arch::asm;

    pub(super) type Word = u32;
    /// MXCSR flush-to-zero (bit 15) and denormals-are-zero (bit 6).
    pub(super) const FLAGS: Word = (1 << 15) | (1 << 6);

    pub(super) fn read() -> Word {
        let mut csr: Word = 0;
        // SAFETY: `stmxcsr` stores the 32-bit MXCSR into the valid, writable `csr`.
        unsafe {
            asm!("stmxcsr [{ptr}]", ptr = in(reg) &mut csr as *mut Word, options(nostack, preserves_flags));
        }
        csr
    }

    pub(super) fn write(csr: Word) {
        // SAFETY: `ldmxcsr` loads MXCSR from the valid `csr`; only rounding/exception-mask and
        // flush bits read back from MXCSR (plus FTZ/DAZ) are written, so no reserved bit is set.
        unsafe {
            asm!("ldmxcsr [{ptr}]", ptr = in(reg) &csr as *const Word, options(nostack, preserves_flags));
        }
    }
}

#[cfg(target_arch = "aarch64")]
mod arch {
    use std::arch::asm;

    pub(super) type Word = u64;
    /// FPCR.FZ (bit 24): flush denormal inputs and outputs to zero.
    pub(super) const FLAGS: Word = 1 << 24;

    pub(super) fn read() -> Word {
        let fpcr: Word;
        // SAFETY: reading FPCR has no side effects.
        unsafe {
            asm!("mrs {}, fpcr", out(reg) fpcr, options(nostack, preserves_flags));
        }
        fpcr
    }

    pub(super) fn write(fpcr: Word) {
        // SAFETY: writes back a value read from FPCR with only the FZ bit possibly added.
        unsafe {
            asm!("msr fpcr, {}", in(reg) fpcr, options(nostack, preserves_flags));
        }
    }
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
mod arch {
    pub(super) type Word = u32;
    pub(super) const FLAGS: Word = 0;

    pub(super) fn read() -> Word {
        0
    }

    pub(super) fn write(_: Word) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::hint::black_box;

    fn halve_smallest_normal() -> f32 {
        black_box(f32::MIN_POSITIVE) * black_box(0.5)
    }

    #[test]
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    fn denormals_flush_inside_the_guard_and_the_mode_is_restored() {
        let before = halve_smallest_normal();
        assert!(before > 0.0, "the test thread starts without FTZ");
        {
            let _guard = DenormalGuard::new();
            assert_eq!(halve_smallest_normal(), 0.0);
        }
        assert_eq!(halve_smallest_normal(), before);
    }
}
