/**
 * The library graph is a derived, in-memory projection — never a persisted
 * store and never a synchronized one. Nodes are saved items and their tags;
 * edges are tag membership plus co-mention, read out of zen document bodies at
 * view time exactly as the reader resolves a single mention.
 *
 * Everything here is pure and synchronous so it can be unit-tested without
 * IndexedDB, and so a renderer swap never touches derivation.
 */

export type GraphNodeKind = "item" | "tag";
export type GraphEdgeKind = "tag" | "mention";

export interface GraphNode {
  /** `item:<uuid>` or `tag:<name>` — unique across both kinds by construction. */
  id: string;
  kind: GraphNodeKind;
  label: string;
  degree: number;
  /** Items only: drives the lit fill. Tags are never favorites. */
  favorite: boolean;
  /** Items only: epoch milliseconds, 0 for a tag hub. */
  savedAt: number;
  /** Tags only: how many active items carry it. Items report 0. */
  count: number;
}

export interface GraphEdge {
  source: string;
  target: string;
  kind: GraphEdgeKind;
  /** Tag membership is always 1; a co-mention counts the documents it spans. */
  weight: number;
}

export interface GraphProjection {
  nodes: GraphNode[];
  edges: GraphEdge[];
  /** Item node ids with degree 0 — saves with no tag and no mention. */
  orphans: string[];
  /** Node id to its neighbours, prebuilt because every frame reads it. */
  adjacency: Map<string, GraphNeighbour[]>;
}

export interface GraphNeighbour {
  id: string;
  kind: GraphEdgeKind;
}

/** The item shape the projection needs — a structural subset of `LibraryItem`. */
export interface GraphItemInput {
  id: string;
  url: string;
  title?: string | null;
  tags: string[];
  favorite: boolean;
  deleted?: boolean;
  savedAt: string;
}

/** One zen document reduced to the distinct items its body mentions. */
export interface GraphMentionSource {
  documentId: string;
  itemIds: string[];
}

export interface GraphProjectionOptions {
  tagEdges?: boolean;
  mentionEdges?: boolean;
  /**
   * A link-dump document mentioning everything would otherwise contribute a
   * clique large enough to dominate the layout, so its fan-out is capped and
   * the excess is reported rather than silently dropped.
   */
  maxComentionFanout?: number;
}

/** 24 mentions is 276 pairs — the point where one document stops being a hub. */
export const MAX_COMENTION_FANOUT = 24;

/** Above this the view asks for a narrower filter instead of melting the device. */
export const GRAPH_NODE_CEILING = 5_000;

/** A phone gets a smaller ceiling; the constellation is already a reduction. */
export const GRAPH_MOBILE_NODE_CEILING = 400;

const MENTION_PREFIX = "research:item/";

/**
 * Pulls the distinct items a zen body mentions.
 *
 * The body is Markdown, so a mention is a link destination rather than a bare
 * token, but scanning for the versioned URI is both cheaper and more forgiving
 * than parsing: a mention inside a reference definition or an autolink counts
 * the same way the reader counts it.
 */
export function extractMentions(body: string): string[] {
  const found: string[] = [];
  const seen = new Set<string>();
  let index = body.indexOf(MENTION_PREFIX);
  while (index !== -1) {
    const start = index + MENTION_PREFIX.length;
    let end = start;
    while (end < body.length && /[0-9a-fA-F-]/.test(body[end]!)) end += 1;
    const candidate = body.slice(start, end).toLowerCase();
    if (isUuid(candidate) && !seen.has(candidate)) {
      seen.add(candidate);
      found.push(candidate);
    }
    index = body.indexOf(MENTION_PREFIX, end);
  }
  return found;
}

function isUuid(value: string): boolean {
  return /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/.test(value);
}

export function itemNodeId(itemId: string): string {
  return `item:${itemId}`;
}

export function tagNodeId(tag: string): string {
  return `tag:${tag}`;
}

/** Recovers the item UUID from a node id, or null for a tag hub. */
export function nodeItemId(nodeId: string): string | null {
  return nodeId.startsWith("item:") ? nodeId.slice(5) : null;
}

/** Recovers the tag name from a node id, or null for an item. */
export function nodeTagName(nodeId: string): string | null {
  return nodeId.startsWith("tag:") ? nodeId.slice(4) : null;
}

/**
 * Builds the projection for a set of items and the documents that mention them.
 *
 * `items` is the list's own result set, so the graph and the list always agree:
 * switching modes changes the projection, never the membership.
 */
export function buildGraphProjection(
  items: GraphItemInput[],
  mentions: GraphMentionSource[] = [],
  options: GraphProjectionOptions = {},
): GraphProjection {
  const tagEdges = options.tagEdges ?? true;
  const mentionEdges = options.mentionEdges ?? true;
  const fanout = options.maxComentionFanout ?? MAX_COMENTION_FANOUT;

  const nodes = new Map<string, GraphNode>();
  const present = new Set<string>();
  for (const item of items) {
    const id = itemNodeId(item.id);
    present.add(item.id);
    nodes.set(id, {
      id,
      kind: "item",
      label: itemLabel(item),
      degree: 0,
      favorite: item.favorite,
      savedAt: readSavedAt(item.savedAt),
      count: 0,
    });
  }

  // Tag hubs exist only when tag membership is being drawn; with the edges off
  // they would stand in the field as unreachable squares.
  if (tagEdges) {
    for (const item of items) {
      for (const tag of item.tags) {
        const id = tagNodeId(tag);
        const existing = nodes.get(id);
        if (existing) {
          existing.count += 1;
          continue;
        }
        nodes.set(id, {
          id,
          kind: "tag",
          label: tag,
          degree: 0,
          favorite: false,
          savedAt: 0,
          count: 1,
        });
      }
    }
  }

  const edges = new Map<string, GraphEdge>();

  if (tagEdges) {
    for (const item of items) {
      for (const tag of item.tags) {
        addEdge(edges, itemNodeId(item.id), tagNodeId(tag), "tag");
      }
    }
  }

  if (mentionEdges) {
    for (const source of mentions) {
      // Only mentions of items in the current result set become edges, or a
      // filtered graph would grow links to saves it is not showing.
      const mentioned = source.itemIds
        .filter((itemId) => present.has(itemId))
        .slice(0, fanout);
      for (let left = 0; left < mentioned.length; left += 1) {
        for (let right = left + 1; right < mentioned.length; right += 1) {
          addEdge(
            edges,
            itemNodeId(mentioned[left]!),
            itemNodeId(mentioned[right]!),
            "mention",
          );
        }
      }
    }
  }

  const adjacency = new Map<string, GraphNeighbour[]>();
  for (const edge of edges.values()) {
    pushNeighbour(adjacency, edge.source, edge.target, edge.kind);
    pushNeighbour(adjacency, edge.target, edge.source, edge.kind);
  }
  for (const node of nodes.values()) {
    node.degree = adjacency.get(node.id)?.length ?? 0;
  }

  const orphans = [...nodes.values()]
    .filter((node) => node.kind === "item" && node.degree === 0)
    .map((node) => node.id);

  return {
    // Sorted so the projection is deterministic: the same library always seeds
    // the same layout, and a test can assert on it.
    nodes: [...nodes.values()].sort(compareNodes),
    edges: [...edges.values()].sort(compareEdges),
    orphans,
    adjacency,
  };
}

/**
 * The mobile reading: tag hubs and the items hanging off them.
 *
 * A phone has no room for the full field, and an untethered item is a dot with
 * no way to say what it is, so the constellation keeps only what a tag names.
 */
export function constellationProjection(
  projection: GraphProjection,
  tagLimit = 7,
): GraphProjection {
  const hubs = projection.nodes
    .filter((node) => node.kind === "tag")
    .sort((left, right) => right.count - left.count || left.label.localeCompare(right.label))
    .slice(0, tagLimit);
  const keep = new Set(hubs.map((node) => node.id));
  for (const hub of hubs) {
    for (const neighbour of projection.adjacency.get(hub.id) ?? []) {
      keep.add(neighbour.id);
    }
  }
  return subgraph(projection, keep);
}

/** Narrows a projection to a node set, recomputing degree and orphans. */
export function subgraph(
  projection: GraphProjection,
  keep: ReadonlySet<string>,
): GraphProjection {
  const edges = projection.edges.filter(
    (edge) => keep.has(edge.source) && keep.has(edge.target),
  );
  const adjacency = new Map<string, GraphNeighbour[]>();
  for (const edge of edges) {
    pushNeighbour(adjacency, edge.source, edge.target, edge.kind);
    pushNeighbour(adjacency, edge.target, edge.source, edge.kind);
  }
  const nodes = projection.nodes
    .filter((node) => keep.has(node.id))
    .map((node) => ({ ...node, degree: adjacency.get(node.id)?.length ?? 0 }));
  return {
    nodes,
    edges,
    orphans: nodes
      .filter((node) => node.kind === "item" && node.degree === 0)
      .map((node) => node.id),
    adjacency,
  };
}

function addEdge(
  edges: Map<string, GraphEdge>,
  left: string,
  right: string,
  kind: GraphEdgeKind,
): void {
  if (left === right) return;
  // Undirected, so the pair is keyed in a stable order and a second document
  // mentioning the same pair thickens the edge instead of duplicating it.
  const [source, target] = left < right ? [left, right] : [right, left];
  const key = `${kind} ${source} ${target}`;
  const existing = edges.get(key);
  if (existing) {
    existing.weight += 1;
    return;
  }
  edges.set(key, { source, target, kind, weight: 1 });
}

function pushNeighbour(
  adjacency: Map<string, GraphNeighbour[]>,
  from: string,
  to: string,
  kind: GraphEdgeKind,
): void {
  const existing = adjacency.get(from);
  if (existing) existing.push({ id: to, kind });
  else adjacency.set(from, [{ id: to, kind }]);
}

function itemLabel(item: GraphItemInput): string {
  const title = item.title?.trim();
  if (title) return title;
  try {
    const url = new URL(item.url);
    return `${url.hostname}${url.pathname === "/" ? "" : url.pathname}`;
  } catch {
    return item.url;
  }
}

function readSavedAt(value: string): number {
  const parsed = Date.parse(value);
  return Number.isNaN(parsed) ? 0 : parsed;
}

function compareNodes(left: GraphNode, right: GraphNode): number {
  if (left.kind !== right.kind) return left.kind === "tag" ? -1 : 1;
  return left.label.localeCompare(right.label) || left.id.localeCompare(right.id);
}

function compareEdges(left: GraphEdge, right: GraphEdge): number {
  return (
    left.kind.localeCompare(right.kind) ||
    left.source.localeCompare(right.source) ||
    left.target.localeCompare(right.target)
  );
}
