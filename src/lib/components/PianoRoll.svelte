<script lang="ts">
  import { tick } from "svelte";
  import { EditQueue } from "../pianoroll/queue";
  import type { SignatureMap } from "../arrange/time";
  import { cssColor } from "../arrange/view";
  import type { Clip, Controller, MidiEdit, MidiEditReply, Note } from "../ipc/types";
  import {
    CC_PRESETS,
    type StrokePoint,
    ccEvents,
    ccPoint,
    ccValueAtY,
    clampController,
    clearController,
    eraseNear,
    paintStroke,
  } from "../pianoroll/cc";
  import {
    allIds,
    deleteNotes,
    duplicateNotes,
    idsAt,
    indicesOf,
    moveNotes,
    scaleVelocities,
  } from "../pianoroll/edit";
  import {
    type Drag,
    type Marquee,
    dragMarquee,
    dragPreview,
    press,
  } from "../pianoroll/gesture";
  import {
    DEFAULT_ROLL_VIEW,
    GRID_OPTIONS,
    KEYS,
    type RollView,
    gridLines,
    hitTest,
    initialScrollTop,
    isBlackKey,
    keyName,
    noteRect,
    rollSize,
    zoomRoll,
  } from "../pianoroll/grid";
  import { type RollKeyAction, ROLL_SHORTCUTS, rollKeyAction } from "../pianoroll/keys";
  import {
    type ClipEdit,
    clipControllers,
    clipNotes,
    noteIds,
    sameControllers,
    sameNotes,
  } from "../pianoroll/notes";
  import { ROOTS, SCALES, rowKind, scaleById } from "../pianoroll/scale";
  import {
    DEFAULT_OPS,
    DEFAULT_PREFS,
    type OpSettings,
    type RollPrefs,
    humanizeEdit,
    legatoEdit,
    nextSeed,
    parsePrefs,
    quantizeEdit,
    transposeEdit,
    velocityScaleEdit,
    velocitySetEdit,
  } from "../pianoroll/toolbar";
  import { barRect, hitBar, velocityAtY } from "../pianoroll/velocity";

  /**
   * The piano roll of one MIDI clip. Draw or select notes (click, shift-click, drag a
   * rectangle), move and resize them (the group together), delete, duplicate and nudge them,
   * edit velocities and a controller lane below the roll, shade a scale, and run quantize,
   * legato, humanize, transpose and velocity operations on the selection (or the whole clip).
   * Every finished gesture is one `onEdit` (one undo step); operations go through `onMidiEdit`.
   * The clip itself only changes when the resulting patch arrives.
   */
  let {
    clip,
    color,
    signatures,
    onEdit,
    onMidiEdit,
    onClose,
  }: {
    clip: Clip;
    /** The track's color (0xRRGGBB). */
    color: number;
    signatures: SignatureMap;
    /** Replaces the clip's notes and/or controller events (one undo step). */
    onEdit: (clip: number, change: ClipEdit) => Promise<boolean | void>;
    /** Runs a note operation on the notes at these indices (all when null). */
    onMidiEdit: (clip: number, edit: MidiEdit, notes: number[] | null) => Promise<MidiEditReply | undefined>;
    onClose: () => void;
  } = $props();

  const ZOOM_STEP = 1.25;
  const LANE_HEIGHT = 56;
  const PREFS_KEY = "openwave.pianoroll.prefs";

  function loadPrefs(): RollPrefs {
    try {
      return parsePrefs(localStorage.getItem(PREFS_KEY));
    } catch {
      return { ...DEFAULT_PREFS };
    }
  }

  let view = $state<RollView>({ ...DEFAULT_ROLL_VIEW });
  let prefs = $state<RollPrefs>(loadPrefs());
  let ops = $state<OpSettings>({ ...DEFAULT_OPS });
  let selected = $state<string[]>([]);
  let drag = $state<{
    drag: Drag;
    preview: { notes: Note[]; selected: string[] } | null;
    base: string[];
  } | null>(null);
  let marquee = $state<Marquee | null>(null);
  let velocityDrag = $state<{
    notes: readonly Note[];
    indices: number[];
    anchor: number;
    preview: Note[] | null;
  } | null>(null);
  let ccDrag = $state<{ preview: Controller[]; last: StrokePoint } | null>(null);
  let cursor = $state("");
  let scroller = $state<HTMLElement>();
  let scrollLeft = $state(0);
  let root = $state<HTMLElement>();
  /** The last press was inside the roll, so its keys act on the roll. */
  let active = $state(false);
  let surface = $state<HTMLElement>();
  let velocitySurface = $state<HTMLElement>();
  let ccSurface = $state<HTMLElement>();
  /** A toolbar operation is waiting for the clip to change, to select the same notes again. */
  let pending = $state.raw<{ before: readonly Note[]; indices: number[] } | null>(null);
  /**
   * What the latest edits set, shown (and built on) while edits are on their way: the patch of
   * an edit arrives after the gesture ended, and a later gesture must not start from the stale
   * clip and overwrite the earlier one. Edits are sent one at a time; gestures that happen
   * while one is in flight are folded into this state and go out as one follow-up edit.
   */
  type Content = { notes: readonly Note[]; controllers: readonly Controller[] };
  let optimistic = $state.raw<Content | null>(null);

  const stored = $derived(clipNotes(clip));
  const storedCc = $derived(clipControllers(clip));
  const committed = $derived(optimistic ? optimistic.notes : stored);
  const committedCc = $derived(optimistic ? optimistic.controllers : storedCc);
  const selectedCount = $derived(indicesOf(committed, new Set(selected)).length);
  const notes = $derived(
    drag
      ? (drag.preview?.notes ?? drag.drag.notes)
      : velocityDrag
        ? (velocityDrag.preview ?? velocityDrag.notes)
        : committed,
  );
  const controllers = $derived(ccDrag ? ccDrag.preview : committedCc);
  const selectedSet = $derived(new Set(selected));
  const shownIds = $derived(noteIds(notes));
  const size = $derived(rollSize(view, clip.length));
  const lines = $derived(gridLines(view, clip.start, clip.length, prefs.grid, signatures));
  const coarse = $derived(lines.filter((l) => l.kind !== "grid"));
  const keys = Array.from({ length: KEYS }, (_, row) => KEYS - 1 - row);
  const scale = $derived(scaleById(prefs.scale));
  const shownCc = $derived(ccEvents(controllers, prefs.controller));
  const scope = $derived(selectedCount > 0 ? `${selectedCount} selected` : "whole clip");
  const ccPresetKnown = $derived(CC_PRESETS.some((p) => p.controller === prefs.controller));

  // Scroll the notes' key range into view when a clip opens (not on every edit).
  let shownClip: number | null = null;
  $effect(() => {
    const id = clip.id;
    const target = scroller;
    if (!target || shownClip === id) return;
    shownClip = id;
    selected = [];
    pending = null;
    optimistic = null;
    void tick().then(() => {
      target.scrollTop = initialScrollTop(committed, view, target.clientHeight);
    });
  });

  // Keep the preferences for the next time the roll opens.
  $effect(() => {
    const snapshot = JSON.stringify($state.snapshot(prefs));
    try {
      localStorage.setItem(PREFS_KEY, snapshot);
    } catch {
      // Browser storage can be unavailable; the roll works without it.
    }
  });

  // A selection never names notes that are gone (undo, redo or an MCP edit removed them).
  $effect(() => {
    if (drag || marquee || velocityDrag) return;
    const present = new Set(noteIds(committed));
    if (selected.some((id) => !present.has(id))) selected = selected.filter((id) => present.has(id));
  });

  // After a toolbar operation changed the clip, select the notes at the same positions again.
  $effect(() => {
    const wait = pending;
    if (wait && stored !== wait.before) {
      selected = idsAt(stored, wait.indices);
      pending = null;
    }
  });

  /** Callbacks of `until`, run whenever the clip's stored content changes. */
  const patchWaiters = new Set<() => void>();
  $effect(() => {
    void stored;
    void storedCc;
    for (const wake of [...patchWaiters]) wake();
    // The queue is idle: whatever changed the clip now (undo, MCP, an operation) is the truth.
    if (!queue.busy && optimistic) optimistic = null;
  });

  /** Resolves true once `done()` holds (checked when the clip changes), false after `limit` ms. */
  function until(done: () => boolean, limit: number): Promise<boolean> {
    return new Promise((resolve) => {
      if (done()) return resolve(true);
      const finish = (result: boolean) => {
        clearTimeout(timer);
        patchWaiters.delete(wake);
        resolve(result);
      };
      const wake = () => done() && finish(true);
      const timer = setTimeout(() => finish(false), limit);
      patchWaiters.add(wake);
    });
  }

  /** Which clips' last edit was seen arriving in the project, until the queue goes idle. */
  const landed = new Map<number, boolean>();

  const queue = new EditQueue<number, Content>(
    async (target, state) => {
      const baseNotes = stored;
      const baseControllers = storedCc;
      // Both halves are sent, so an edit of one never reverts an earlier edit of the other.
      const accepted = await onEdit(target, { notes: state.notes, controllers: state.controllers });
      if (accepted === false) return false;
      const seen = await until(
        () => clip.id !== target || stored !== baseNotes || storedCc !== baseControllers,
        1000,
      );
      landed.set(target, seen);
      return true;
    },
    {
      onIdle: () => {
        const mine = optimistic;
        if (landed.get(clip.id) === true) optimistic = null;
        else {
          // The patch was not seen: keep the result shown for a moment, then trust the clip.
          setTimeout(() => {
            if (optimistic === mine && !queue.busy) optimistic = null;
          }, 400);
        }
        landed.clear();
      },
      // A refused edit never arrived: show the clip as it is.
      onFail: (target) => {
        if (target === clip.id) optimistic = null;
      },
    },
  );

  /** Queues an edit (unless it changes nothing) and shows its result at once. */
  function commit(change: ClipEdit) {
    const notes = change.notes && !sameNotes(change.notes, committed) ? change.notes : undefined;
    const controllers =
      change.controllers && !sameControllers(change.controllers, committedCc) ? change.controllers : undefined;
    if (!notes && !controllers) return;
    const next = { notes: notes ?? committed, controllers: controllers ?? committedCc };
    optimistic = next;
    queue.push(clip.id, next);
  }

  function surfacePoint(target: HTMLElement | undefined, event: { clientX: number; clientY: number }) {
    const rect = target!.getBoundingClientRect();
    return { x: event.clientX - rect.left, y: event.clientY - rect.top };
  }

  // ---- The note surface ----

  function onPointerDown(event: PointerEvent) {
    if (event.button !== 0 || !surface) return;
    pending = null;
    const { x, y } = surfacePoint(surface, event);
    const result = press(committed, selectedSet, view, x, y, {
      tool: prefs.tool,
      grid: prefs.grid,
      clipLength: clip.length,
      shift: event.shiftKey,
      ctrl: event.ctrlKey || event.metaKey,
    });
    selected = result.selection;
    if (result.type === "add") {
      commit({ notes: result.notes });
    } else if (result.type === "drag") {
      surface.setPointerCapture(event.pointerId);
      drag = { drag: result.drag, preview: null, base: result.selection };
    } else if (result.type === "marquee") {
      surface.setPointerCapture(event.pointerId);
      marquee = result.marquee;
    }
  }

  function onPointerMove(event: PointerEvent) {
    if (!surface) return;
    const { x, y } = surfacePoint(surface, event);
    if (drag) {
      const preview = dragPreview(drag.drag, view, x, y, prefs.grid, clip.length);
      drag = { ...drag, preview };
      selected = preview ? preview.selected : drag.base;
    } else if (marquee) {
      const next = dragMarquee(committed, view, marquee, x, y);
      marquee = next.marquee;
      selected = next.selection;
    } else {
      const hit = hitTest(committed, view, x, y);
      cursor = hit ? (hit.part === "body" ? "grab" : "ew-resize") : prefs.tool === "draw" ? "crosshair" : "default";
    }
  }

  function onPointerUp(event: PointerEvent) {
    marquee = null;
    if (!drag) return;
    const done = drag;
    drag = null;
    if (event.type !== "pointerup") {
      selected = done.base;
    } else if (done.preview) {
      commit({ notes: done.preview.notes });
    } else if (done.drag.clickSelection) {
      selected = done.drag.clickSelection;
    }
  }

  function onContextMenu(event: MouseEvent) {
    event.preventDefault();
    if (!surface) return;
    const { x, y } = surfacePoint(surface, event);
    const hit = hitTest(committed, view, x, y);
    if (hit) {
      selected = [];
      commit({ notes: deleteNotes(committed, [hit.index]) });
    }
  }

  // ---- The velocity lane ----

  function onVelocityDown(event: PointerEvent) {
    if (event.button !== 0 || !velocitySurface) return;
    pending = null;
    const { x, y } = surfacePoint(velocitySurface, event);
    const hit = hitBar(committed, view, x, y, LANE_HEIGHT);
    if (hit === null) return;
    const id = noteIds(committed)[hit]!;
    if (event.shiftKey && !selectedSet.has(id)) selected = [...selected, id];
    else if (!selectedSet.has(id)) selected = [id];
    const indices = indicesOf(committed, new Set(selected));
    velocitySurface.setPointerCapture(event.pointerId);
    velocityDrag = { notes: committed, indices, anchor: hit, preview: null };
    onVelocityMove(event);
  }

  function onVelocityMove(event: PointerEvent) {
    if (!velocityDrag || !velocitySurface) return;
    const { y } = surfacePoint(velocitySurface, event);
    const preview = scaleVelocities(
      velocityDrag.notes,
      velocityDrag.indices,
      velocityDrag.anchor,
      velocityAtY(y, LANE_HEIGHT),
    );
    velocityDrag = { ...velocityDrag, preview };
  }

  function onVelocityUp(event: PointerEvent) {
    if (!velocityDrag) return;
    const done = velocityDrag;
    velocityDrag = null;
    if (event.type === "pointerup" && done.preview) commit({ notes: done.preview });
  }

  // ---- The controller lane ----

  function strokePoint(event: PointerEvent): StrokePoint {
    const { x, y } = surfacePoint(ccSurface, event);
    return { beat: Math.max(0, x / view.pxPerBeat), value: ccValueAtY(y, LANE_HEIGHT) };
  }

  function onCcDown(event: PointerEvent) {
    if (event.button !== 0 || !ccSurface) return;
    ccSurface.setPointerCapture(event.pointerId);
    const to = strokePoint(event);
    ccDrag = {
      preview: paintStroke(committedCc, prefs.controller, null, to, prefs.grid, clip.length),
      last: to,
    };
  }

  function onCcMove(event: PointerEvent) {
    if (!ccDrag || !ccSurface) return;
    const to = strokePoint(event);
    ccDrag = {
      preview: paintStroke(ccDrag.preview, prefs.controller, ccDrag.last, to, prefs.grid, clip.length),
      last: to,
    };
  }

  function onCcUp(event: PointerEvent) {
    if (!ccDrag) return;
    const done = ccDrag;
    ccDrag = null;
    if (event.type === "pointerup") commit({ controllers: done.preview });
  }

  function onCcContextMenu(event: MouseEvent) {
    event.preventDefault();
    if (!ccSurface) return;
    const { x } = surfacePoint(ccSurface, event);
    const next = eraseNear(committedCc, prefs.controller, view, x);
    if (next) commit({ controllers: next });
  }

  // ---- Operations ----

  /** Runs a note operation on the selection (the whole clip when nothing is selected). */
  async function runOp(edit: MidiEdit) {
    // Edits still on their way come first, so the indices below name the same notes there.
    const target = clip.id;
    await queue.idle();
    optimistic = null;
    if (clip.id !== target) return;
    const before = stored;
    const indices = indicesOf(committed, selectedSet);
    const reply = await onMidiEdit(target, edit, indices.length > 0 ? indices : null);
    if (indices.length === 0 || !reply) return;
    // Operations keep the order of the notes, so the same positions are the same notes (unless
    // a transpose dropped some, then the selection ends).
    if (reply.notesAfter !== reply.notesBefore) {
      selected = [];
      return;
    }
    pending = { before, indices };
    // An operation that changed nothing sends no new content: stop waiting for it.
    setTimeout(() => {
      if (pending?.before === before) pending = null;
    }, 500);
  }

  async function runHumanize() {
    const edit = humanizeEdit(ops);
    ops.seed = nextSeed(ops.seed);
    await runOp(edit);
  }

  // ---- Keys ----

  function nudge(deltaBeats: number, deltaKeys: number) {
    const indices = indicesOf(committed, selectedSet);
    if (indices.length === 0) return;
    const result = moveNotes(committed, indices, indices[0]!, deltaBeats, deltaKeys, prefs.grid, clip.length);
    if (!result) return;
    selected = result.selected;
    commit({ notes: result.notes });
  }

  function perform(action: RollKeyAction): boolean {
    const indices = indicesOf(committed, selectedSet);
    switch (action) {
      case "delete":
        if (indices.length === 0) return false;
        selected = [];
        commit({ notes: deleteNotes(committed, indices) });
        return true;
      case "select-all":
        selected = allIds(committed);
        return true;
      case "duplicate": {
        const result = duplicateNotes(committed, indices, prefs.grid, clip.length);
        if (!result) return indices.length > 0;
        selected = result.selected;
        commit({ notes: result.notes });
        return true;
      }
      case "deselect":
        if (selected.length === 0) return false;
        selected = [];
        return true;
      case "nudge-left":
      case "nudge-right":
      case "nudge-up":
      case "nudge-down":
      case "octave-up":
      case "octave-down": {
        if (indices.length === 0) return false;
        const beats = action === "nudge-left" ? -prefs.grid : action === "nudge-right" ? prefs.grid : 0;
        const semis =
          action === "nudge-up" ? 1 : action === "nudge-down" ? -1 : action === "octave-up" ? 12 : action === "octave-down" ? -12 : 0;
        nudge(beats, semis);
        return true;
      }
      case "tool-draw":
        prefs.tool = "draw";
        return true;
      case "tool-select":
        prefs.tool = "select";
        return true;
      case "quantize":
        void runOp(quantizeEdit(prefs.grid, ops));
        return true;
      case "legato":
        void runOp(legatoEdit(ops));
        return true;
    }
  }

  function onKey(event: KeyboardEvent) {
    const target = event.target as HTMLElement | null;
    if (target?.closest("input, textarea, select, [contenteditable]") || !active) return;
    const action = rollKeyAction(event);
    if (action && !(event.repeat && (action === "duplicate" || action === "quantize" || action === "legato"))) {
      if (perform(action)) {
        event.preventDefault();
        event.stopPropagation();
      }
    }
  }
</script>

<svelte:window
  onkeydowncapture={onKey}
  onpointerdowncapture={(event) => (active = root?.contains(event.target as Node) ?? false)}
/>

<section class="roll" aria-label="Piano roll" style:--track={cssColor(color)} bind:this={root}>
  <header>
    <strong>{clip.name}</strong>
    <span class="tools" role="group" aria-label="Tool">
      <button
        type="button"
        class:on={prefs.tool === "draw"}
        aria-pressed={prefs.tool === "draw"}
        title="Draw notes (D)"
        onclick={() => (prefs.tool = "draw")}>Draw</button
      >
      <button
        type="button"
        class:on={prefs.tool === "select"}
        aria-pressed={prefs.tool === "select"}
        title="Select notes with a rectangle (V)"
        onclick={() => (prefs.tool = "select")}>Select</button
      >
    </span>
    <label>
      Grid
      <select bind:value={prefs.grid} aria-label="Snap grid">
        {#each GRID_OPTIONS as option (option.beats)}
          <option value={option.beats}>{option.label}</option>
        {/each}
      </select>
    </label>
    <label>
      Scale
      <select bind:value={prefs.root} aria-label="Scale root">
        {#each ROOTS as name, index (name)}
          <option value={index}>{name}</option>
        {/each}
      </select>
      <select bind:value={prefs.scale} aria-label="Scale">
        {#each SCALES as option (option.id)}
          <option value={option.id}>{option.label}</option>
        {/each}
      </select>
    </label>
    <span class="spacer"></span>
    <span class="hint" title={ROLL_SHORTCUTS.map((s) => `${s.keys}: ${s.label}`).join("\n")}>Keys</span>
    <button type="button" aria-label="Zoom out" onclick={() => (view = zoomRoll(view, 1 / ZOOM_STEP))}>&minus;</button>
    <button type="button" aria-label="Zoom in" onclick={() => (view = zoomRoll(view, ZOOM_STEP))}>+</button>
    <button type="button" onclick={onClose}>Close</button>
  </header>

  <div class="ops" role="group" aria-label="Note operations">
    <span class="scope" aria-live="polite">On: {scope}</span>
    <span class="op">
      <button type="button" title="Move note starts to the grid (Q)" onclick={() => runOp(quantizeEdit(prefs.grid, ops))}>Quantize</button>
      <label>
        <input type="number" min="0" max="100" step="5" bind:value={ops.strength} aria-label="Quantize strength percent" />%
      </label>
      <label><input type="checkbox" bind:checked={ops.lengths} /> lengths</label>
    </span>
    <span class="op">
      <button type="button" title="Stretch notes to the next note (L)" onclick={() => runOp(legatoEdit(ops))}>Legato</button>
      <select bind:value={ops.legato} aria-label="Legato mode">
        <option value="any_key">any key</option>
        <option value="same_key">same key</option>
      </select>
    </span>
    <span class="op">
      <button type="button" title="Random, repeatable timing and velocity changes" onclick={runHumanize}>Humanize</button>
      <label>
        t <input type="number" min="0" max="1" step="0.01" bind:value={ops.timing} aria-label="Humanize timing in beats" />
      </label>
      <label>
        v <input type="number" min="0" max="127" step="1" bind:value={ops.velocity} aria-label="Humanize velocity range" />
      </label>
      <label>
        seed <input type="number" min="0" step="1" bind:value={ops.seed} aria-label="Humanize seed" />
      </label>
    </span>
    <span class="op">
      Transpose
      <button type="button" aria-label="Down an octave" onclick={() => runOp(transposeEdit(-12))}>-12</button>
      <button type="button" aria-label="Down a semitone" onclick={() => runOp(transposeEdit(-1))}>-1</button>
      <button type="button" aria-label="Up a semitone" onclick={() => runOp(transposeEdit(1))}>+1</button>
      <button type="button" aria-label="Up an octave" onclick={() => runOp(transposeEdit(12))}>+12</button>
    </span>
    <span class="op">
      Velocity
      <input type="number" min="1" max="127" step="1" bind:value={ops.setTo} aria-label="Velocity to set" />
      <button type="button" onclick={() => runOp(velocitySetEdit(ops))}>Set</button>
      <input type="number" min="1" max="400" step="5" bind:value={ops.scalePercent} aria-label="Velocity scale percent" />%
      <button type="button" onclick={() => runOp(velocityScaleEdit(ops))}>Scale</button>
    </span>
  </div>

  <div class="scroll" bind:this={scroller} onscroll={() => (scrollLeft = scroller?.scrollLeft ?? 0)}>
    <div class="inner" style:width="{size.width + 48}px" style:height="{size.height}px">
      <div class="keys" aria-hidden="true">
        {#each keys as key (key)}
          {@const kind = rowKind(key, prefs.root, scale)}
          <div
            class="key"
            class:black={isBlackKey(key)}
            class:in-scale={kind === "scale"}
            class:root-key={kind === "root"}
            style:height="{view.pxPerKey}px"
            style:line-height="{view.pxPerKey}px"
          >
            {key % 12 === 0 ? keyName(key) : ""}
          </div>
        {/each}
      </div>
      <!-- svelte-ignore a11y_no_static_element_interactions -->
      <div
        class="surface"
        bind:this={surface}
        style:width="{size.width}px"
        style:height="{size.height}px"
        style:cursor={cursor || (prefs.tool === "draw" ? "crosshair" : "default")}
        onpointerdown={onPointerDown}
        onpointermove={onPointerMove}
        onpointerup={onPointerUp}
        onpointercancel={onPointerUp}
        oncontextmenu={onContextMenu}
      >
        {#each keys as key (key)}
          {@const kind = rowKind(key, prefs.root, scale)}
          <div
            class="row {kind}"
            class:black={kind === "plain" && isBlackKey(key)}
            style:top="{(KEYS - 1 - key) * view.pxPerKey}px"
            style:height="{view.pxPerKey}px"
          ></div>
        {/each}
        {#each lines as line (line.beat)}
          <div class="line {line.kind}" style:left="{line.x}px"></div>
        {/each}
        {#each notes as note, index (shownIds[index])}
          {@const rect = noteRect(view, note)}
          <div
            class="note"
            class:selected={selectedSet.has(shownIds[index]!)}
            style:left="{rect.x}px"
            style:top="{rect.y}px"
            style:width="{rect.width}px"
            style:height="{rect.height}px"
            style:opacity={0.5 + (0.5 * note.velocity) / 127}
            role="presentation"
          ></div>
        {/each}
        {#if marquee}
          <div
            class="marquee"
            style:left="{Math.min(marquee.x0, marquee.x1)}px"
            style:top="{Math.min(marquee.y0, marquee.y1)}px"
            style:width="{Math.abs(marquee.x1 - marquee.x0)}px"
            style:height="{Math.abs(marquee.y1 - marquee.y0)}px"
          ></div>
        {/if}
      </div>
    </div>
  </div>

  <div class="lane" aria-label="Velocity lane">
    <div class="lane-head">
      <span>Velocity</span>
      <span class="hint">drag a bar; selected notes scale together</span>
    </div>
    <div class="lane-body" style:height="{LANE_HEIGHT}px">
      <div class="gutter"></div>
      <div class="lane-view">
        <!-- svelte-ignore a11y_no_static_element_interactions -->
        <div
          class="lane-surface"
          bind:this={velocitySurface}
          style:width="{size.width}px"
          style:transform="translateX({-scrollLeft}px)"
          onpointerdown={onVelocityDown}
          onpointermove={onVelocityMove}
          onpointerup={onVelocityUp}
          onpointercancel={onVelocityUp}
        >
          {#each coarse as line (line.beat)}
            <div class="line {line.kind}" style:left="{line.x}px"></div>
          {/each}
          {#each notes as note, index (shownIds[index])}
            {@const bar = barRect(view, note, LANE_HEIGHT)}
            <div
              class="bar"
              class:selected={selectedSet.has(shownIds[index]!)}
              style:left="{bar.x}px"
              style:top="{bar.y}px"
              style:width="{bar.width}px"
              style:height="{bar.height}px"
              title={String(note.velocity)}
            ></div>
          {/each}
        </div>
      </div>
    </div>
  </div>

  <div class="lane" aria-label="Controller lane">
    <div class="lane-head">
      <select
        aria-label="Controller"
        value={ccPresetKnown ? prefs.controller : -1}
        onchange={(event) => {
          const value = Number(event.currentTarget.value);
          if (value >= 0) prefs.controller = value;
        }}
      >
        {#each CC_PRESETS as preset (preset.controller)}
          <option value={preset.controller}>CC {preset.controller} {preset.name}</option>
        {/each}
        {#if !ccPresetKnown}
          <option value={-1}>CC {prefs.controller}</option>
        {/if}
      </select>
      <label>
        CC
        <input
          type="number"
          min="0"
          max="127"
          step="1"
          value={prefs.controller}
          aria-label="Controller number"
          onchange={(event) => (prefs.controller = clampController(Number(event.currentTarget.value)))}
        />
      </label>
      <span class="hint">{shownCc.length} events; drag to draw, right-click a point to erase</span>
      <button
        type="button"
        disabled={shownCc.length === 0}
        onclick={() => commit({ controllers: clearController(committedCc, prefs.controller) })}>Clear</button
      >
    </div>
    <div class="lane-body" style:height="{LANE_HEIGHT}px">
      <div class="gutter"></div>
      <div class="lane-view">
        <!-- svelte-ignore a11y_no_static_element_interactions -->
        <div
          class="lane-surface cc"
          bind:this={ccSurface}
          style:width="{size.width}px"
          style:transform="translateX({-scrollLeft}px)"
          onpointerdown={onCcDown}
          onpointermove={onCcMove}
          onpointerup={onCcUp}
          onpointercancel={onCcUp}
          oncontextmenu={onCcContextMenu}
        >
          {#each coarse as line (line.beat)}
            <div class="line {line.kind}" style:left="{line.x}px"></div>
          {/each}
          {#each shownCc as event, index (index)}
            {@const point = ccPoint(view, event, LANE_HEIGHT)}
            <div
              class="ccbar"
              style:left="{point.x}px"
              style:top="{point.y}px"
              style:height="{LANE_HEIGHT - point.y}px"
              title={String(event.value)}
            ></div>
            <div class="ccdot" style:left="{point.x}px" style:top="{point.y}px"></div>
          {/each}
        </div>
      </div>
    </div>
  </div>
</section>

<style>
  .roll {
    display: flex;
    flex-direction: column;
    flex: 1;
    min-width: 0;
    min-height: 0;
    background: var(--bg);
  }
  header,
  .ops {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 8px;
    padding: 3px 8px;
    background: var(--panel);
    border-bottom: 1px solid var(--border);
    font-size: 12px;
  }
  .ops {
    background: var(--panel-2);
    gap: 6px 14px;
  }
  .op {
    display: inline-flex;
    align-items: center;
    gap: 4px;
  }
  .ops input[type="number"] {
    width: 54px;
  }
  .scope,
  .hint {
    color: var(--muted);
  }
  .tools {
    display: inline-flex;
  }
  .tools button.on {
    background: var(--accent);
    color: var(--bg);
  }
  .spacer {
    flex: 1;
  }
  .scroll {
    flex: 1;
    min-height: 100px;
    overflow: auto;
  }
  .inner {
    position: relative;
    display: flex;
  }
  .keys {
    position: sticky;
    left: 0;
    z-index: 2;
    flex: none;
    width: 48px;
    background: var(--panel);
    border-right: 1px solid var(--border);
  }
  .key {
    box-sizing: border-box;
    padding-left: 4px;
    font-size: 9px;
    color: var(--muted);
    border-bottom: 1px solid var(--border);
    background: var(--panel);
  }
  .key.black {
    background: var(--panel-2);
  }
  .key.in-scale {
    box-shadow: inset 4px 0 0 color-mix(in srgb, var(--accent) 45%, transparent);
  }
  .key.root-key {
    box-shadow: inset 4px 0 0 var(--accent);
    color: var(--accent);
  }
  .surface {
    position: relative;
    flex: none;
    touch-action: none;
    user-select: none;
  }
  .row {
    position: absolute;
    left: 0;
    right: 0;
    box-sizing: border-box;
    border-bottom: 1px solid color-mix(in srgb, var(--border) 50%, transparent);
    pointer-events: none;
  }
  .row.black {
    background: color-mix(in srgb, var(--panel-2) 60%, transparent);
  }
  .row.scale {
    background: color-mix(in srgb, var(--accent) 9%, transparent);
  }
  .row.root {
    background: color-mix(in srgb, var(--accent) 20%, transparent);
  }
  .row.out {
    background: color-mix(in srgb, black 22%, transparent);
  }
  .line {
    position: absolute;
    top: 0;
    bottom: 0;
    width: 0;
    border-left: 1px solid color-mix(in srgb, var(--border) 45%, transparent);
    pointer-events: none;
  }
  .line.beat {
    border-left-color: color-mix(in srgb, var(--border) 85%, transparent);
  }
  .line.bar {
    border-left-color: var(--muted);
  }
  .note {
    position: absolute;
    box-sizing: border-box;
    border: 1px solid color-mix(in srgb, var(--track) 70%, black);
    border-radius: 2px;
    background: var(--track);
    pointer-events: none;
  }
  .note.selected {
    outline: 2px solid var(--accent);
    outline-offset: -1px;
    filter: brightness(1.25);
  }
  .marquee {
    position: absolute;
    box-sizing: border-box;
    border: 1px dashed var(--accent);
    background: color-mix(in srgb, var(--accent) 15%, transparent);
    pointer-events: none;
  }
  .lane {
    flex: none;
    border-top: 1px solid var(--border);
  }
  .lane-head {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 2px 8px;
    height: 22px;
    box-sizing: border-box;
    background: var(--panel);
    font-size: 11px;
  }
  .lane-head input[type="number"] {
    width: 54px;
  }
  .lane-body {
    display: flex;
  }
  .gutter {
    flex: none;
    width: 48px;
    background: var(--panel);
    border-right: 1px solid var(--border);
  }
  .lane-view {
    position: relative;
    flex: 1;
    min-width: 0;
    overflow: hidden;
  }
  .lane-surface {
    position: relative;
    height: 100%;
    cursor: ns-resize;
    touch-action: none;
    user-select: none;
    background: color-mix(in srgb, var(--panel-2) 40%, transparent);
  }
  .lane-surface.cc {
    cursor: crosshair;
  }
  .bar {
    position: absolute;
    box-sizing: border-box;
    border-radius: 2px 2px 0 0;
    background: color-mix(in srgb, var(--track) 80%, transparent);
    pointer-events: none;
  }
  .bar.selected {
    background: var(--accent);
  }
  .ccbar {
    position: absolute;
    width: 2px;
    margin-left: -1px;
    background: color-mix(in srgb, var(--accent) 70%, transparent);
    pointer-events: none;
  }
  .ccdot {
    position: absolute;
    width: 7px;
    height: 7px;
    margin: -3px 0 0 -3px;
    border-radius: 50%;
    background: var(--accent);
    pointer-events: none;
  }
</style>
