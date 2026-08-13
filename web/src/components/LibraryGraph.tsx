import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type KeyboardEvent,
} from "react";
import { GraphLayout, type LayoutBody } from "../data/graphLayout.ts";
import {
  buildGraphProjection,
  constellationProjection,
  subgraph,
  nodeItemId,
  nodeTagName,
  GRAPH_MOBILE_NODE_CEILING,
  GRAPH_NODE_CEILING,
  type GraphItemInput,
  type GraphMentionSource,
  type GraphNode,
  type GraphProjection,
} from "../data/graph.ts";

export interface LibraryGraphProps {
  /** The list's own result set — the graph never shows a different membership. */
  items: GraphItemInput[];
  mentions: GraphMentionSource[];
  /** Paused rather than unmounted, so a mode switch keeps the settled layout. */
  hidden: boolean;
  busy: boolean;
  selectedTags: string[];
  onOpenItem(itemId: string): void;
  onEditItem(itemId: string, opener: HTMLButtonElement): void;
  onFavoriteItem(itemId: string): void;
  onDeleteItem(itemId: string): void;
  onRestoreItem(itemId: string): void;
  onFilterTag(tag: string): void;
}

interface Palette {
  canvas: string;
  item: string;
  favorite: string;
  orphan: string;
  tag: string;
  tagEdge: string;
  mentionEdge: string;
  ring: string;
  pin: string;
  label: string;
  labelStrong: string;
  tagLabel: string;
}

// Low enough that a thousand-node cloud frames whole. The old 0.25 floor was
// set for the prototype's forty nodes and quietly clipped anything larger.
const MIN_SCALE = 0.08;
const MAX_SCALE = 4;
const MOBILE_QUERY = "(max-width: 48rem)";
const INSPECTOR_OPEN_STORAGE_KEY = "researchpocket.ui.graph-inspector-open";

export function LibraryGraph({
  items,
  mentions,
  hidden,
  busy,
  selectedTags,
  onOpenItem,
  onEditItem,
  onFavoriteItem,
  onDeleteItem,
  onRestoreItem,
  onFilterTag,
}: LibraryGraphProps) {
  const [showTags, setShowTags] = useState(true);
  const [showMentions, setShowMentions] = useState(true);
  const [orphansOnly, setOrphansOnly] = useState(false);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [zoom, setZoom] = useState(1);
  const [pinnedCount, setPinnedCount] = useState(0);
  const [legendOpen, setLegendOpen] = useState(false);
  const [inspectorOpen, setInspectorOpen] = useState(() =>
    readInspectorPreference(),
  );
  const [narrow, setNarrow] = useState(() => matchesNarrow());
  const [unavailable, setUnavailable] = useState(false);
  const canvasRef = useRef<HTMLCanvasElement | null>(null);

  const full = useMemo(
    () =>
      buildGraphProjection(items, mentions, {
        tagEdges: showTags,
        mentionEdges: showMentions,
      }),
    [items, mentions, showMentions, showTags],
  );

  // The orphan filter and the phone's constellation are both narrowings of the
  // one projection, so degree and orphan counts stay honest in each reading.
  const projection = useMemo(() => {
    if (orphansOnly) return subgraph(full, new Set(full.orphans));
    if (narrow) return constellationProjection(full);
    return full;
  }, [full, narrow, orphansOnly]);

  const ceiling = narrow ? GRAPH_MOBILE_NODE_CEILING : GRAPH_NODE_CEILING;
  const overCeiling = projection.nodes.length > ceiling;
  const drawable = !overCeiling && !unavailable;

  const selected = selectedId
    ? (projection.nodes.find((node) => node.id === selectedId) ?? null)
    : null;
  const selectedItem = selected ? sourceItem(selected, items) : undefined;

  // The inspector is what a selection is for, so selecting brings it back.
  // Without this, closing it once would strand every later selection in a
  // panel nobody can see — and the toolbar no longer carries a way to reopen.
  const selectNode = useCallback((id: string | null) => {
    setSelectedId(id);
    if (id) setInspectorOpen(true);
  }, []);

  // Degree order puts the hubs first, which is the order someone stepping
  // through the graph by keyboard actually wants to meet it in.
  const traversal = useMemo(
    () =>
      [...projection.nodes].sort(
        (left, right) => right.degree - left.degree || left.label.localeCompare(right.label),
      ),
    [projection.nodes],
  );

  const simulation = useRef<Simulation | null>(null);

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const created = createSimulation(canvas, {
      onSelect: (id) => selectNode(id),
      onScale: (value) => setZoom(value),
      onPinnedChange: (count) => setPinnedCount(count),
      onUnavailable: () => setUnavailable(true),
    });
    if (!created) {
      setUnavailable(true);
      return;
    }
    simulation.current = created;
    return () => {
      created.destroy();
      simulation.current = null;
    };
  }, []);

  useEffect(() => {
    try {
      window.localStorage.setItem(
        INSPECTOR_OPEN_STORAGE_KEY,
        String(inspectorOpen),
      );
    } catch {
      // The preference remains active for this tab when storage is unavailable.
    }
  }, [inspectorOpen]);

  useEffect(() => {
    const media = window.matchMedia?.(MOBILE_QUERY);
    const onNarrow = () => setNarrow(matchesNarrow());
    media?.addEventListener("change", onNarrow);
    return () => {
      media?.removeEventListener("change", onNarrow);
    };
  }, []);

  useEffect(() => {
    simulation.current?.configure({
      projection: overCeiling ? EMPTY_PROJECTION : projection,
      narrow,
    });
  }, [narrow, overCeiling, projection]);

  useEffect(() => {
    simulation.current?.setSelection(selectedId);
  }, [selectedId]);

  // Not the active mode, or not the visible tab: the loop stops entirely rather
  // than drifting against a canvas nobody is looking at.
  useEffect(() => {
    const update = () =>
      simulation.current?.setRunning(!hidden && !document.hidden);
    update();
    document.addEventListener("visibilitychange", update);
    return () => document.removeEventListener("visibilitychange", update);
  }, [hidden]);

  // A selection that the filter just removed would leave the inspector
  // describing something no longer on screen.
  useEffect(() => {
    if (selectedId && !projection.nodes.some((node) => node.id === selectedId)) {
      setSelectedId(null);
    }
  }, [projection.nodes, selectedId]);

  const cursors = useRef(new Map<string, number>());

  function handleCameraKeyDown(event: KeyboardEvent<HTMLDivElement>) {
    if (!drawable || event.ctrlKey || event.metaKey || event.altKey) return;
    if (event.target !== event.currentTarget) return;
    if (event.key === "0") {
      event.preventDefault();
      simulation.current?.reset();
    } else if (event.key === "+" || event.key === "=") {
      event.preventDefault();
      simulation.current?.zoomBy(1.25);
    } else if (event.key === "-" || event.key === "_") {
      event.preventDefault();
      simulation.current?.zoomBy(0.8);
    } else if (event.key.toLowerCase() === "c" && selectedId) {
      event.preventDefault();
      simulation.current?.focus(selectedId);
    }
  }

  function openSelection(node: GraphNode) {
    const itemId = nodeItemId(node.id);
    if (itemId) {
      const item = items.find((candidate) => candidate.id === itemId);
      if (item && !item.deleted) onOpenItem(itemId);
      return;
    }
    const tag = nodeTagName(node.id);
    if (tag) onFilterTag(tag);
  }

  /** Arrow keys walk the neighbours of whatever has focus. */
  function stepNeighbour(node: GraphNode, direction: 1 | -1) {
    const neighbours = projection.adjacency.get(node.id) ?? [];
    if (neighbours.length === 0) return;
    const cursor = (cursors.current.get(node.id) ?? -1) + direction;
    const index = ((cursor % neighbours.length) + neighbours.length) % neighbours.length;
    cursors.current.set(node.id, index);
    const target = neighbours[index]!.id;
    selectNode(target);
    simulation.current?.center(target);
    document.getElementById(nodeButtonId(target))?.focus();
  }

  // The node list is one button per node — up to the ceiling — so it must not
  // re-render for state the canvas owns. Zoom alone changes on every wheel
  // tick, and reconciling thousands of buttons against it would cost more than
  // the frame it is competing with. Handlers go through a ref so the list can
  // depend on the projection and nothing else.
  const actions = useRef({ openSelection, stepNeighbour, select: selectNode });
  actions.current = { openSelection, stepNeighbour, select: selectNode };

  const nodeList = useMemo(
    () => (
      <ul
        aria-label="Graph nodes in degree order"
        className={unavailable ? "graph-nodes" : "graph-nodes sr-only"}
      >
        {traversal.map((node) => (
          <li key={node.id}>
            <button
              id={nodeButtonId(node.id)}
              onClick={() => {
                actions.current.select(node.id);
                simulation.current?.center(node.id);
              }}
              onFocus={() => actions.current.select(node.id)}
              onKeyDown={(event) => {
                if (event.key === "Enter" || event.key === " ") {
                  event.preventDefault();
                  actions.current.openSelection(node);
                  return;
                }
                if (event.key === "ArrowDown" || event.key === "ArrowRight") {
                  event.preventDefault();
                  actions.current.stepNeighbour(node, 1);
                  return;
                }
                if (event.key === "ArrowUp" || event.key === "ArrowLeft") {
                  event.preventDefault();
                  actions.current.stepNeighbour(node, -1);
                }
              }}
              type="button"
            >
              {describeNode(node)}
            </button>
          </li>
        ))}
      </ul>
    ),
    [traversal, unavailable],
  );

  return (
    <div className="library-graph">
      {/* Filters only. The camera and the key belong to the drawing, and they
          live on it — a toolbar carrying both plus a panel toggle was ten
          controls deep and scrolled sideways before the filters were reached. */}
      <div className="graph-toolbar">
        <button
          aria-pressed={showTags}
          onClick={() => setShowTags((value) => !value)}
          type="button"
        >
          tag links · {showTags ? "on" : "off"}
        </button>
        <button
          aria-pressed={showMentions}
          onClick={() => setShowMentions((value) => !value)}
          type="button"
        >
          co-mentions · {showMentions ? "on" : "off"}
        </button>
        <button
          aria-pressed={orphansOnly}
          onClick={() => setOrphansOnly((value) => !value)}
          type="button"
        >
          orphans · {orphansOnly ? "only" : full.orphans.length}
        </button>
        {pinnedCount > 0 ? (
          <button
            className="graph-release"
            onClick={() => simulation.current?.releaseAll()}
            type="button"
          >
            release {pinnedCount} pinned
          </button>
        ) : null}
      </div>

      <div
        className={`graph-body${!narrow && !inspectorOpen ? " graph-body-inspector-closed" : ""}`}
      >
        <div
          aria-describedby={drawable ? "graph-camera-shortcuts" : undefined}
          aria-label={drawable ? "Graph canvas" : undefined}
          className="graph-stage"
          onKeyDown={handleCameraKeyDown}
          tabIndex={!hidden && drawable ? 0 : -1}
        >
          <p className="sr-only" id="graph-camera-shortcuts">
            When this graph has focus, plus and minus zoom, 0 fits all nodes,
            and C focuses the selected node.
          </p>
          <canvas
            aria-hidden="true"
            className="graph-canvas"
            hidden={overCeiling || unavailable}
            ref={canvasRef}
          />

          {overCeiling ? (
            <p className="graph-notice">
              <strong>
                {projection.nodes.length.toLocaleString()} nodes is more than the
                graph draws at once.
              </strong>
              <span>
                Narrow the result set — a search, a tag, or favourites — and the
                layout will re-form around what is left. The list shows all of
                them either way.
              </span>
            </p>
          ) : null}

          {unavailable && !overCeiling ? (
            <p className="graph-notice">
              <strong>This browser could not open a drawing surface.</strong>
              <span>
                The graph is listed below instead: every node, in degree order,
                with the same neighbourhoods.
              </span>
            </p>
          ) : null}

          {!overCeiling && !unavailable ? (
            <>
              <p className="graph-stats">
                <span>{projection.nodes.length.toLocaleString()} nodes</span>
                <span aria-hidden="true">·</span>
                <span>{projection.edges.length.toLocaleString()} edges</span>
                <span aria-hidden="true">·</span>
                <span className="graph-orphan-count">
                  {full.orphans.length.toLocaleString()} orphans
                </span>
              </p>

              {legendOpen ? (
                <dl className="graph-legend" id="graph-legend">
                  <div>
                    <dt aria-hidden="true" className="graph-key graph-key-item">
                      ●
                    </dt>
                    <dd>saved item</dd>
                  </div>
                  <div>
                    <dt aria-hidden="true" className="graph-key graph-key-favorite">
                      ●
                    </dt>
                    <dd>favorite</dd>
                  </div>
                  <div>
                    <dt aria-hidden="true" className="graph-key graph-key-tag">
                      ■
                    </dt>
                    <dd>tag hub</dd>
                  </div>
                  <div>
                    <dt className="graph-key graph-key-tag-edge" />
                    <dd>tag membership</dd>
                  </div>
                  <div>
                    <dt className="graph-key graph-key-mention-edge" />
                    <dd>co-mentioned in a Zen doc</dd>
                  </div>
                </dl>
              ) : null}

              {/* Its own toggle sits under it, on the drawing it explains. */}
              <button
                aria-controls="graph-legend"
                aria-expanded={legendOpen}
                className="graph-key-toggle"
                onClick={() => setLegendOpen((value) => !value)}
                type="button"
              >
                key
              </button>

              <div aria-label="Graph camera" className="graph-camera" role="group">
                <button
                  aria-label="Zoom out"
                  onClick={() => simulation.current?.zoomBy(0.8)}
                  type="button"
                >
                  −
                </button>
                <output aria-label="Zoom level">{Math.round(zoom * 100)}%</output>
                <button
                  aria-label="Zoom in"
                  onClick={() => simulation.current?.zoomBy(1.25)}
                  type="button"
                >
                  +
                </button>
                <button onClick={() => simulation.current?.reset()} type="button">
                  reset
                </button>
              </div>
            </>
          ) : null}

          {/* The canvas is not the only route through the graph. This list is
              the keyboard's, and it is what stands in for the drawing when a
              browser cannot give us one. What is selected is announced by the
              inspector, which is the accessible name for the selection. */}
          {nodeList}
        </div>

        <aside
          className={`graph-inspector${!narrow && !inspectorOpen ? " graph-inspector-closed" : ""}`}
          id="graph-inspector"
        >
          {/* The right of this row used to hold the kind label, which rendered
              as a bare em dash with nothing selected — a dash sitting exactly
              where a close button belongs, which is what it got clicked as.
              The label moved left; the slot now holds the control it looked
              like. Selecting anything opens the panel again. */}
          <div className="graph-inspector-heading">
            <p>{selected ? kindLabel(selected) : "Selected"}</p>
            {!narrow ? (
              <button
                aria-controls="graph-inspector"
                aria-expanded={inspectorOpen}
                className="graph-inspector-close"
                onClick={() => setInspectorOpen(false)}
                title="Close details"
                type="button"
              >
                <span aria-hidden="true">×</span>
                <span className="sr-only">Close details</span>
              </button>
            ) : null}
          </div>

          {/* The selection is announced here rather than marked on the node
              list, so keyboard traversal reads the save it lands on. */}
          <div aria-live="polite" className="graph-inspector-subject" role="status">
            <p className="graph-inspector-title">
              {selected ? nodeTitle(selected) : "Nothing selected"}
            </p>
            <p className="graph-inspector-meta">
              {selected ? nodeMeta(selected, items) : "Choose a node to see its details"}
            </p>
          </div>

          {selected ? (
            <div className="graph-inspector-tags">
              {tagsFor(selected, items).map((tag) => (
                <button
                  aria-label={`#${tag}, ${selectedTags.includes(tag) ? "remove" : "add"} tag filter`}
                  aria-pressed={selectedTags.includes(tag)}
                  key={tag}
                  onClick={() => onFilterTag(tag)}
                  type="button"
                >
                  #{tag}
                </button>
              ))}
            </div>
          ) : null}

          <div className="graph-inspector-actions">
            {selectedItem ? (
              <a
                className="primary-button"
                href={selectedItem.url}
                rel="noreferrer"
                target="_blank"
              >
                Open original
              </a>
            ) : (
              <button
                className="primary-button"
                disabled={!selected}
                onClick={() => selected && openSelection(selected)}
                type="button"
              >
                Filter
              </button>
            )}
            <button
              className="secondary-button"
              disabled={!selected}
              onClick={() =>
                selectedItem && !selectedItem.deleted
                  ? onOpenItem(selectedItem.id)
                  : selected && simulation.current?.center(selected.id)
              }
              type="button"
            >
              {selectedItem && !selectedItem.deleted ? "Reader" : "Center"}
            </button>
          </div>

          {selectedItem ? (
            <div
              aria-label={`Actions for ${nodeTitle(selected!)}`}
              className="graph-quick-actions"
              role="group"
            >
              {selectedItem.deleted ? (
                <button
                  disabled={busy}
                  onClick={() => onRestoreItem(selectedItem.id)}
                  type="button"
                >
                  <span aria-hidden="true">↶</span> Restore
                </button>
              ) : (
                <>
                  <button
                    aria-pressed={selectedItem.favorite}
                    disabled={busy}
                    onClick={() => onFavoriteItem(selectedItem.id)}
                    type="button"
                  >
                    <span aria-hidden="true">★</span>{" "}
                    {selectedItem.favorite ? "Unfavorite" : "Favorite"}
                  </button>
                  <button
                    aria-haspopup="dialog"
                    disabled={busy}
                    onClick={(event) => onEditItem(selectedItem.id, event.currentTarget)}
                    type="button"
                  >
                    <span aria-hidden="true">✎</span> Edit details
                  </button>
                  <button
                    className="graph-quick-action-danger"
                    disabled={busy}
                    onClick={() => onDeleteItem(selectedItem.id)}
                    type="button"
                  >
                    <span aria-hidden="true">⌫</span> Archive
                  </button>
                </>
              )}
            </div>
          ) : null}

          <div className="graph-neighbourhood">
            <div className="graph-inspector-heading">
              <p>Neighbourhood</p>
              <span>{selected ? `${selected.degree} links` : "0"}</span>
            </div>
            <ol>
              {(selected ? (projection.adjacency.get(selected.id) ?? []) : [])
                .slice(0, 24)
                .map((neighbour, index) => {
                  const target = projection.nodes.find(
                    (node) => node.id === neighbour.id,
                  );
                  if (!target) return null;
                  return (
                    <li key={`${neighbour.id}-${index}`}>
                      <button
                        onClick={() => {
                          setSelectedId(target.id);
                          simulation.current?.center(target.id);
                        }}
                        type="button"
                      >
                        <span
                          aria-hidden="true"
                          className={`graph-glyph graph-glyph-${neighbour.kind === "mention" ? "mention" : target.kind}`}
                        >
                          {neighbour.kind === "mention"
                            ? "◇"
                            : target.kind === "tag"
                              ? "■"
                              : "●"}
                        </span>
                        <span>{nodeTitle(target)}</span>
                      </button>
                    </li>
                  );
                })}
            </ol>
          </div>
        </aside>
      </div>
    </div>
  );
}

const EMPTY_PROJECTION: GraphProjection = {
  nodes: [],
  edges: [],
  orphans: [],
  adjacency: new Map(),
};

function matchesNarrow(): boolean {
  return window.matchMedia?.(MOBILE_QUERY).matches ?? false;
}

function readInspectorPreference(): boolean {
  try {
    return window.localStorage.getItem(INSPECTOR_OPEN_STORAGE_KEY) !== "false";
  } catch {
    return true;
  }
}

function nodeButtonId(id: string): string {
  return `graph-node-${id.replace(/[^a-zA-Z0-9_-]/g, "_")}`;
}

function nodeTitle(node: GraphNode): string {
  return node.kind === "tag" ? `#${node.label}` : node.label;
}

function kindLabel(node: GraphNode): string {
  if (node.kind === "tag") return "tag";
  return node.degree === 0 ? "orphan" : "saved item";
}

function describeNode(node: GraphNode): string {
  return `${nodeTitle(node)} — ${kindLabel(node)}, ${node.degree} ${node.degree === 1 ? "link" : "links"}`;
}

function sourceItem(node: GraphNode, items: GraphItemInput[]) {
  const itemId = nodeItemId(node.id);
  return itemId ? items.find((item) => item.id === itemId) : undefined;
}

function tagsFor(node: GraphNode, items: GraphItemInput[]): string[] {
  return sourceItem(node, items)?.tags ?? [];
}

function nodeMeta(node: GraphNode, items: GraphItemInput[]): string {
  if (node.kind === "tag") {
    return `${node.count.toLocaleString()} ${node.count === 1 ? "save carries" : "saves carry"} this tag · tag hub`;
  }
  const item = sourceItem(node, items);
  if (!item) return "saved item";
  const saved = node.savedAt
    ? new Intl.DateTimeFormat(undefined, { day: "numeric", month: "short" }).format(
        new Date(node.savedAt),
      )
    : "recently";
  return `${hostname(item.url)} · saved ${saved}${item.favorite ? " · ★ favorite" : ""}`;
}

function hostname(url: string): string {
  try {
    return new URL(url).hostname;
  } catch {
    return "saved link";
  }
}

/* ------------------------------------------------------------------ *
 * Simulation and renderer
 *
 * Canvas 2D rather than WebGL: every node treatment the design asks for —
 * the square tag hub, the favourite fill, the selection ring, the pinned ring
 * — is a shader program under a WebGL renderer and two lines of context calls
 * here, and the projection above stays renderer-agnostic either way.
 * ------------------------------------------------------------------ */

interface SimulationHandlers {
  onSelect(id: string | null): void;
  onScale(scale: number): void;
  onPinnedChange(count: number): void;
  onUnavailable(): void;
}

interface SimulationConfig {
  projection: GraphProjection;
  narrow: boolean;
}

interface Simulation {
  configure(config: SimulationConfig): void;
  setSelection(id: string | null): void;
  setRunning(running: boolean): void;
  center(id: string): void;
  focus(id: string): void;
  zoomBy(factor: number): void;
  reset(): void;
  releaseAll(): void;
  destroy(): void;
}

function createSimulation(
  canvas: HTMLCanvasElement,
  handlers: SimulationHandlers,
): Simulation | null {
  let context = canvas.getContext("2d");
  if (!context) return null;

  const layout = new GraphLayout();
  const bodies = layout.bodies;
  let nodesById = new Map<string, GraphNode>();
  let projection: GraphProjection = EMPTY_PROJECTION;
  let narrow = false;
  let palette = readPalette();
  let width = 0;
  let height = 0;
  let dpr = window.devicePixelRatio || 1;
  let scale = 1;
  let translateX = 0;
  let translateY = 0;
  let hover: string | null = null;
  let selection: string | null = null;
  let ticks = 0;
  let frozen = false;
  let running = false;
  let fitPending = true;
  let warmed = false;
  /** The running loop's handle, kept apart from the one-shot redraw's. */
  let frame = 0;
  let drawFrame = 0;
  let fontsReady = false;
  let drag: {
    body: LayoutBody;
    dx: number;
    dy: number;
    startX: number;
    startY: number;
    moved: boolean;
  } | null = null;
  let pan: { x: number; y: number } | null = null;
  const pointers = new Map<number, { x: number; y: number }>();
  let pinch: { distance: number; scale: number } | null = null;

  void document.fonts?.ready.then(() => {
    fontsReady = true;
    requestDraw();
  });

  /* ---- transform ---- */

  const toWorld = (clientX: number, clientY: number) => {
    const rect = canvas.getBoundingClientRect();
    return {
      x: (clientX - rect.left - width / 2 - translateX) / scale,
      y: (clientY - rect.top - height / 2 - translateY) / scale,
    };
  };

  const project = (body: LayoutBody) => ({
    x: body.x * scale + width / 2 + translateX,
    y: body.y * scale + height / 2 + translateY,
  });

  function pick(worldX: number, worldY: number): LayoutBody | null {
    let best: LayoutBody | null = null;
    let bestDistance = Infinity;
    for (const body of layout.order) {
      const dx = body.x - worldX;
      const dy = body.y - worldY;
      const distance = dx * dx + dy * dy;
      const reach = (body.radius + 8 / scale) ** 2;
      if (distance < reach && distance < bestDistance) {
        best = body;
        bestDistance = distance;
      }
    }
    return best;
  }

  /* ---- sizing ---- */

  const resize = () => {
    const rect = canvas.getBoundingClientRect();
    if (!rect.width || !rect.height) return;
    width = rect.width;
    height = rect.height;
    dpr = window.devicePixelRatio || 1;
    canvas.width = Math.round(rect.width * dpr);
    canvas.height = Math.round(rect.height * dpr);
    if (!warmed) fitPending = true;
    thaw();
  };
  const observer = new ResizeObserver(resize);
  observer.observe(canvas);
  resize();

  /* ---- lifecycle ---- */

  function thaw() {
    // Any interaction un-freezes a settled layout for long enough to redraw.
    frozen = false;
    if (!running) {
      requestDraw();
      return;
    }
    if (frame === 0) frame = requestAnimationFrame(loop);
  }

  /** A single frame, for a paused or settled graph that still has to repaint. */
  function requestDraw() {
    if (drawFrame !== 0) return;
    drawFrame = requestAnimationFrame(() => {
      drawFrame = 0;
      draw();
    });
  }

  function loop() {
    frame = 0;
    if (!running) return;

    // Soft walls only once the view has been framed, or a layout that has not
    // spread yet is clamped into the opening viewport and never escapes it.
    const moving = layout.step({
      narrow,
      dragging: drag?.body.id ?? null,
      limitX: warmed ? Math.max(60, (width / 2 - 26) / scale) : undefined,
      limitY: warmed ? Math.max(60, (height / 2 - 26) / scale) : undefined,
    });
    ticks += 1;
    if (fitPending && ticks > 8) fit();

    draw();

    // Once the cooled layout reports settled, there is no reason to keep
    // spending frames or moving pixels.
    if (!moving) frozen = true;
    if (!frozen) frame = requestAnimationFrame(loop);
  }

  /* ---- framing ---- */

  function fit() {
    const box = layout.bounds();
    if (!box || !width) return;
    const { minX, minY, maxX, maxY } = box;
    // Labels are drawn to the right of their node and are not in the bounds,
    // so the frame is padded for them. A phone shows tag labels only, but it
    // is also where there is least room for one to run off the edge.
    const pad = narrow ? 64 : 96;
    const boundsWidth = Math.max(60, maxX - minX);
    const boundsHeight = Math.max(60, maxY - minY);
    const next = Math.max(
      MIN_SCALE,
      Math.min((width - pad * 2) / boundsWidth, (height - pad * 2) / boundsHeight, 1.4),
    );
    scale = next;
    translateX = -((minX + maxX) / 2) * scale;
    translateY = -((minY + maxY) / 2) * scale;
    fitPending = false;
    warmed = true;
    handlers.onScale(scale);
  }

  /* ---- drawing ---- */

  function draw() {
    if (!context || !width) return;
    context.setTransform(dpr, 0, 0, dpr, 0, 0);
    context.clearRect(0, 0, width, height);
    context.fillStyle = palette.canvas;
    context.fillRect(0, 0, width, height);

    const focus = hover ?? selection;
    const near = new Set<string>();
    if (focus) {
      near.add(focus);
      for (const neighbour of projection.adjacency.get(focus) ?? []) {
        near.add(neighbour.id);
      }
    }
    // Only a hover dims the field. A selection that dimmed everything else
    // would leave the graph unreadable for as long as something is selected.
    const dimming = hover !== null;

    context.lineWidth = 1;
    for (const edge of projection.edges) {
      const from = bodies.get(edge.source);
      const to = bodies.get(edge.target);
      if (!from || !to) continue;
      const lit = !dimming || (near.has(edge.source) && near.has(edge.target));
      context.strokeStyle =
        edge.kind === "mention" ? palette.mentionEdge : palette.tagEdge;
      context.globalAlpha = lit ? (edge.kind === "mention" ? 0.8 : 0.55) : 0.12;
      const a = project(from);
      const b = project(to);
      context.beginPath();
      context.moveTo(a.x, a.y);
      context.lineTo(b.x, b.y);
      context.stroke();
    }
    context.globalAlpha = 1;

    const labels: {
      node: GraphNode;
      x: number;
      y: number;
      radius: number;
      strong: boolean;
    }[] = [];
    for (const body of layout.order) {
      const node = nodesById.get(body.id);
      if (!node) continue;
      const point = project(body);
      const lit = !dimming || near.has(body.id);
      const isSelected = body.id === selection;
      const radius = Math.max(2.5, body.radius * Math.min(1.4, scale));
      context.globalAlpha = lit ? 1 : 0.18;

      if (node.kind === "tag") {
        context.fillStyle = palette.tag;
        context.fillRect(point.x - radius, point.y - radius, radius * 2, radius * 2);
        context.strokeStyle = isSelected ? palette.ring : palette.canvas;
        context.lineWidth = isSelected ? 2 : 1;
        context.strokeRect(point.x - radius, point.y - radius, radius * 2, radius * 2);
      } else {
        context.beginPath();
        context.arc(point.x, point.y, radius, 0, Math.PI * 2);
        context.fillStyle = node.favorite
          ? palette.favorite
          : node.degree > 0
            ? palette.item
            : palette.orphan;
        context.fill();
        if (isSelected) {
          context.strokeStyle = palette.ring;
          context.lineWidth = 2;
          context.beginPath();
          context.arc(point.x, point.y, radius + 3.5, 0, Math.PI * 2);
          context.stroke();
        }
        if (body.pinned) {
          context.strokeStyle = palette.pin;
          context.lineWidth = 1;
          context.beginPath();
          context.arc(point.x, point.y, radius + 2, 0, Math.PI * 2);
          context.stroke();
        }
      }

      // Labels are the expensive part, so they are rationed: tag hubs always
      // (they are what makes a cluster readable), items only when they are the
      // subject or the view is zoomed in far enough to have room.
      const wanted =
        node.kind === "tag"
          ? true
          : !narrow &&
            (body.id === focus || body.id === selection || (!dimming && scale > 1.5));
      if (wanted && (!dimming || near.has(body.id))) {
        labels.push({
          node,
          x: point.x,
          y: point.y,
          radius,
          strong: body.id === focus || body.id === selection,
        });
      }
    }
    context.globalAlpha = 1;

    if (fontsReady) {
      context.textBaseline = "middle";
      for (const label of labels) {
        const text = nodeTitle(label.node);
        const limit = narrow ? 16 : 42;
        const clipped = text.length > limit ? `${text.slice(0, limit - 1)}…` : text;
        context.font = `${label.strong ? "700 " : ""}11px "TX-02", ui-monospace, monospace`;
        const measured = context.measureText(clipped).width;
        // A plate behind the text, or a label crossing three edges is unreadable.
        context.fillStyle = palette.canvas;
        context.globalAlpha = 0.82;
        context.fillRect(label.x + label.radius + 4, label.y - 7, measured + 6, 14);
        context.globalAlpha = 1;
        context.fillStyle =
          label.node.kind === "tag"
            ? palette.tagLabel
            : label.strong
              ? palette.labelStrong
              : palette.label;
        context.fillText(clipped, label.x + label.radius + 7, label.y);
      }
    }
  }

  /* ---- pointer input ---- */

  const onPointerDown = (event: PointerEvent) => {
    canvas.parentElement?.focus({ preventScroll: true });
    pointers.set(event.pointerId, { x: event.clientX, y: event.clientY });
    if (pointers.size === 2) {
      pinch = { distance: pointerDistance(), scale };
      drag = null;
      pan = null;
      return;
    }
    canvas.setPointerCapture(event.pointerId);
    const point = toWorld(event.clientX, event.clientY);
    const hit = pick(point.x, point.y);
    if (hit) {
      selection = hit.id;
      handlers.onSelect(hit.id);
      // Drag-to-pin is a mouse gesture. On a phone the same press is a tap to
      // read, and a finger that wanders must not silently pin the node.
      if (!narrow) {
        drag = {
          body: hit,
          dx: hit.x - point.x,
          dy: hit.y - point.y,
          startX: event.clientX,
          startY: event.clientY,
          moved: false,
        };
      }
    } else {
      pan = { x: event.clientX - translateX, y: event.clientY - translateY };
    }
    thaw();
  };

  const onPointerMove = (event: PointerEvent) => {
    if (pointers.has(event.pointerId)) {
      pointers.set(event.pointerId, { x: event.clientX, y: event.clientY });
    }
    if (pinch && pointers.size === 2) {
      const distance = pointerDistance();
      if (pinch.distance > 0) {
        setScale((pinch.scale * distance) / pinch.distance);
      }
      return;
    }
    if (drag) {
      if (
        !drag.moved &&
        Math.hypot(event.clientX - drag.startX, event.clientY - drag.startY) < 3
      ) {
        return;
      }
      if (!drag.moved) layout.reheat(0.18);
      const point = toWorld(event.clientX, event.clientY);
      drag.body.x = point.x + drag.dx;
      drag.body.y = point.y + drag.dy;
      drag.body.vx = 0;
      drag.body.vy = 0;
      drag.moved = true;
      thaw();
      return;
    }
    if (pan) {
      translateX = event.clientX - pan.x;
      translateY = event.clientY - pan.y;
      thaw();
      return;
    }
    const point = toWorld(event.clientX, event.clientY);
    const hit = pick(point.x, point.y);
    const next = hit?.id ?? null;
    if (next !== hover) {
      hover = next;
      canvas.style.cursor = hit ? "pointer" : "default";
      thaw();
    }
  };

  const onPointerUp = (event: PointerEvent) => {
    pointers.delete(event.pointerId);
    if (pointers.size < 2) pinch = null;
    if (canvas.hasPointerCapture(event.pointerId)) {
      canvas.releasePointerCapture(event.pointerId);
    }
    if (drag?.moved) {
      drag.body.pinned = true;
      reportPinned();
    }
    drag = null;
    pan = null;
  };

  const onPointerLeave = () => {
    if (hover !== null) {
      hover = null;
      thaw();
    }
  };

  const onDoubleClick = (event: MouseEvent) => {
    const point = toWorld(event.clientX, event.clientY);
    const hit = pick(point.x, point.y);
    if (hit?.pinned) {
      hit.pinned = false;
      reportPinned();
      layout.reheat(0.25);
      thaw();
    }
  };

  const onWheel = (event: WheelEvent) => {
    event.preventDefault();
    // Anchored at the pointer, so zooming reads as moving toward what is under
    // the cursor rather than toward the middle of the panel.
    const rect = canvas.getBoundingClientRect();
    const anchorX = event.clientX - rect.left - width / 2;
    const anchorY = event.clientY - rect.top - height / 2;
    const worldX = (anchorX - translateX) / scale;
    const worldY = (anchorY - translateY) / scale;
    const next = clampScale(scale * (event.deltaY < 0 ? 1.08 : 0.93));
    translateX = anchorX - worldX * next;
    translateY = anchorY - worldY * next;
    scale = next;
    handlers.onScale(next);
    thaw();
  };

  const onContextLost = (event: Event) => {
    event.preventDefault();
    frozen = true;
  };

  const onContextRestored = () => {
    context = canvas.getContext("2d");
    if (!context) {
      handlers.onUnavailable();
      return;
    }
    palette = readPalette();
    thaw();
  };

  function pointerDistance(): number {
    const [first, second] = [...pointers.values()];
    if (!first || !second) return 0;
    return Math.hypot(first.x - second.x, first.y - second.y);
  }

  function setScale(value: number) {
    scale = clampScale(value);
    handlers.onScale(scale);
    thaw();
  }

  function reportPinned() {
    let count = 0;
    for (const body of layout.order) if (body.pinned) count += 1;
    handlers.onPinnedChange(count);
  }

  canvas.addEventListener("pointerdown", onPointerDown);
  canvas.addEventListener("pointermove", onPointerMove);
  canvas.addEventListener("pointerup", onPointerUp);
  canvas.addEventListener("pointercancel", onPointerUp);
  canvas.addEventListener("pointerleave", onPointerLeave);
  canvas.addEventListener("dblclick", onDoubleClick);
  canvas.addEventListener("wheel", onWheel, { passive: false });
  canvas.addEventListener("contextlost", onContextLost);
  canvas.addEventListener("contextrestored", onContextRestored);

  // A theme change rewrites custom properties on the root element; the canvas
  // has no cascade to inherit from, so it re-reads them when they move.
  const themeObserver = new MutationObserver(() => {
    palette = readPalette();
    thaw();
  });
  themeObserver.observe(document.documentElement, {
    attributeFilter: ["style"],
  });

  return {
    configure(config) {
      const topologyChanged =
        narrow !== config.narrow || !sameTopology(projection, config.projection);
      projection = config.projection;
      narrow = config.narrow;
      nodesById = new Map(projection.nodes.map((node) => [node.id, node]));

      // Favorite, title, and URL edits only change paint or inspector copy.
      // Re-running the force layout for them would make a direct action move
      // the graph even though its topology did not change.
      if (!topologyChanged) {
        thaw();
        return;
      }

      // The layout reconciles the body set: a surviving node keeps its
      // position and velocity, a filtered node leaves the simulation rather
      // than hiding, and the whole thing reheats around what is left.
      layout.sync(
        projection.nodes.map((node) => ({ id: node.id, radius: radiusFor(node) })),
        projection.edges,
      );
      // Settled before anything is drawn, and framed once around the result:
      // no reframe lands mid-flight, and the walls below are measured against
      // a cloud that has already finished growing.
      layout.settle({ narrow });
      warmed = true;
      fitPending = true;
      fit();
      ticks = 0;
      reportPinned();
      thaw();
    },
    setSelection(id) {
      selection = id;
      thaw();
    },
    setRunning(value) {
      if (running === value) return;
      running = value;
      if (running) {
        frozen = false;
        if (frame === 0) frame = requestAnimationFrame(loop);
      } else if (frame !== 0) {
        cancelAnimationFrame(frame);
        frame = 0;
      }
    },
    center(id) {
      const body = bodies.get(id);
      if (!body) return;
      translateX = -body.x * scale;
      translateY = -body.y * scale;
      thaw();
    },
    focus(id) {
      const body = bodies.get(id);
      if (!body) return;
      scale = Math.max(scale, 1);
      translateX = -body.x * scale;
      translateY = -body.y * scale;
      handlers.onScale(scale);
      thaw();
    },
    zoomBy(factor) {
      setScale(scale * factor);
    },
    reset() {
      fitPending = true;
      warmed = true;
      fit();
      thaw();
    },
    releaseAll() {
      for (const body of layout.order) body.pinned = false;
      reportPinned();
      layout.reheat(0.35);
      thaw();
    },
    destroy() {
      if (frame !== 0) cancelAnimationFrame(frame);
      if (drawFrame !== 0) cancelAnimationFrame(drawFrame);
      frame = 0;
      drawFrame = 0;
      running = false;
      observer.disconnect();
      themeObserver.disconnect();
      canvas.removeEventListener("pointerdown", onPointerDown);
      canvas.removeEventListener("pointermove", onPointerMove);
      canvas.removeEventListener("pointerup", onPointerUp);
      canvas.removeEventListener("pointercancel", onPointerUp);
      canvas.removeEventListener("pointerleave", onPointerLeave);
      canvas.removeEventListener("dblclick", onDoubleClick);
      canvas.removeEventListener("wheel", onWheel);
      canvas.removeEventListener("contextlost", onContextLost);
      canvas.removeEventListener("contextrestored", onContextRestored);
      layout.sync([], []);
    },
  };
}

function clampScale(value: number): number {
  return Math.min(MAX_SCALE, Math.max(MIN_SCALE, value));
}

function radiusFor(node: GraphNode): number {
  // Tag hubs are count-weighted so a cluster's weight is legible before its
  // label is; items stay uniform, because a save is a save.
  return node.kind === "tag" ? 5 + Math.min(5, node.count * 0.6) : 4.5;
}

function sameTopology(left: GraphProjection, right: GraphProjection): boolean {
  if (left.nodes.length !== right.nodes.length || left.edges.length !== right.edges.length) {
    return false;
  }
  const rightNodes = new Map(right.nodes.map((node) => [node.id, node]));
  for (const leftNode of left.nodes) {
    const rightNode = rightNodes.get(leftNode.id);
    if (!rightNode || radiusFor(leftNode) !== radiusFor(rightNode)) {
      return false;
    }
  }
  for (let index = 0; index < left.edges.length; index += 1) {
    const leftEdge = left.edges[index]!;
    const rightEdge = right.edges[index]!;
    if (
      leftEdge.source !== rightEdge.source ||
      leftEdge.target !== rightEdge.target ||
      leftEdge.kind !== rightEdge.kind ||
      leftEdge.weight !== rightEdge.weight
    ) {
      return false;
    }
  }
  return true;
}

/* ---- palette ---- */

/**
 * The canvas has no cascade, so every colour is resolved out of the token
 * system through a probe element. There is no literal anywhere in this file,
 * which is what keeps a custom theme and `prefers-contrast: more` reaching the
 * drawing the same way they reach the rest of the application.
 */
function readPalette(): Palette {
  const probe = document.createElement("span");
  probe.style.display = "none";
  document.body.append(probe);
  // An expression the browser rejects leaves the inherited colour in place,
  // so a missing token degrades to readable text rather than to transparent.
  const resolve = (expression: string) => {
    probe.style.color = "";
    probe.style.color = expression;
    return getComputedStyle(probe).color;
  };
  const palette: Palette = {
    canvas: resolve("var(--color-canvas)"),
    item: resolve("var(--color-text-soft)"),
    favorite: resolve("var(--color-accent-strong)"),
    orphan: resolve("var(--color-border-strong)"),
    tag: resolve("var(--color-accent)"),
    tagEdge: resolve("var(--color-border)"),
    mentionEdge: resolve("var(--accent)"),
    ring: resolve("var(--color-text)"),
    pin: resolve("var(--color-accent-strong)"),
    label: resolve("var(--color-text-soft)"),
    labelStrong: resolve("var(--color-text)"),
    tagLabel: resolve("color-mix(in srgb, var(--color-accent) 55%, var(--color-text))"),
  };
  probe.remove();
  return palette;
}
