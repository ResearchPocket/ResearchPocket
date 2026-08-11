import assert from "node:assert/strict";
import { test } from "node:test";

import { GraphLayout, seedPosition } from "./graphLayout.ts";

/** A library-shaped graph: items around tag hubs, a few co-mentions. */
function fixture(itemCount, tagCount = Math.ceil(itemCount / 12)) {
  const nodes = [];
  const edges = [];
  for (let tag = 0; tag < tagCount; tag += 1) {
    nodes.push({ id: `tag:t${tag}`, radius: 7 });
  }
  for (let item = 0; item < itemCount; item += 1) {
    nodes.push({ id: `item:i${item}`, radius: 4.5 });
    edges.push({
      source: `item:i${item}`,
      target: `tag:t${item % tagCount}`,
      kind: "tag",
      weight: 1,
    });
    // A second tag for every third save, so clusters actually overlap.
    if (item % 3 === 0) {
      edges.push({
        source: `item:i${item}`,
        target: `tag:t${(item * 7) % tagCount}`,
        kind: "tag",
        weight: 1,
      });
    }
    if (item % 25 === 0 && item > 0) {
      edges.push({
        source: `item:i${item}`,
        target: `item:i${item - 25}`,
        kind: "mention",
        weight: 1,
      });
    }
  }
  return { nodes, edges };
}

function run(layout, ticks) {
  let moving = 0;
  for (let tick = 0; tick < ticks; tick += 1) {
    if (layout.step()) moving = tick + 1;
    else break;
  }
  return moving;
}

test("a thousand-save library comes to rest", () => {
  const { nodes, edges } = fixture(1000);
  const layout = new GraphLayout();
  layout.sync(nodes, edges);
  assert.equal(layout.order.length, 1000 + Math.ceil(1000 / 12));

  const ticks = run(layout, 1200);

  // The defect this test exists for: with no alpha the layout ran at full
  // energy forever, so every node moved at the speed cap indefinitely.
  assert.ok(layout.settled, `layout never settled (ran ${ticks} ticks)`);
  assert.ok(ticks < 1200, "settling must not need the whole budget");

  for (const body of layout.order) {
    assert.ok(Number.isFinite(body.x) && Number.isFinite(body.y), "position diverged");
  }

  // Bounded, not exploded: a settled thousand-node cloud is large but finite.
  const bounds = layout.bounds();
  const width = bounds.maxX - bounds.minX;
  const height = bounds.maxY - bounds.minY;
  assert.ok(width > 200 && height > 200, "the cloud collapsed to a point");
  assert.ok(width < 40_000 && height < 40_000, `the cloud exploded: ${width}x${height}`);
});

test("the layout is at rest before the first frame even when its budget expires", () => {
  const { nodes, edges } = fixture(1000);
  const layout = new GraphLayout();
  layout.sync(nodes, edges);

  // A busy device can exhaust the synchronous budget before cooling naturally.
  // The renderer still has to paint a stable graph rather than finish live.
  const ticks = layout.settle({}, 600, 0);
  assert.ok(layout.settled, `settle() returned unsettled after ${ticks} ticks`);

  let furthest = 0;
  const before = layout.order.map((body) => ({ x: body.x, y: body.y }));
  for (let tick = 0; tick < 30; tick += 1) layout.step();
  layout.order.forEach((body, index) => {
    furthest = Math.max(
      furthest,
      Math.hypot(body.x - before[index].x, body.y - before[index].y),
    );
  });
  assert.ok(furthest < 1, `first painted frames moved ${furthest.toFixed(2)}px`);
});

test("handling one node nudges the layout instead of restarting it", () => {
  const { nodes, edges } = fixture(1000);
  const layout = new GraphLayout();
  layout.sync(nodes, edges);
  layout.settle({}, 600, Number.POSITIVE_INFINITY);

  // Grabbing a save must not re-energise the whole field to opening amplitude.
  layout.reheat(0.25);
  assert.equal(layout.alpha, 0.25);

  // Long enough for alpha to fall from the nudge back under the floor.
  const before = layout.order.map((body) => ({ x: body.x, y: body.y }));
  for (let tick = 0; tick < 260; tick += 1) layout.step();
  let furthest = 0;
  layout.order.forEach((body, index) => {
    furthest = Math.max(
      furthest,
      Math.hypot(body.x - before[index].x, body.y - before[index].y),
    );
  });
  assert.ok(furthest < 60, `a nudge moved the field ${furthest.toFixed(0)}px`);
  assert.ok(layout.settled, "a nudge must come back to rest");
});

test("a settled layout stays still when it is stepped again", () => {
  const { nodes, edges } = fixture(600);
  const layout = new GraphLayout();
  layout.sync(nodes, edges);
  run(layout, 1200);
  assert.ok(layout.settled);

  const before = layout.order.map((body) => ({ x: body.x, y: body.y }));
  for (let tick = 0; tick < 60; tick += 1) layout.step();

  // A second of frames must not move a settled large graph perceptibly.
  let furthest = 0;
  layout.order.forEach((body, index) => {
    furthest = Math.max(furthest, Math.hypot(body.x - before[index].x, body.y - before[index].y));
  });
  assert.ok(furthest < 1, `settled layout drifted ${furthest.toFixed(2)}px in 60 ticks`);
});

test("small and large libraries both come to rest", () => {
  const small = new GraphLayout();
  const smallFixture = fixture(40, 5);
  small.sync(smallFixture.nodes, smallFixture.edges);
  run(small, 1200);
  assert.equal(small.settled, true);

  const large = new GraphLayout();
  const largeFixture = fixture(1000);
  large.sync(largeFixture.nodes, largeFixture.edges);
  run(large, 1200);
  assert.equal(large.settled, true);
});

test("a filter change keeps surviving positions and re-forms around them", () => {
  const layout = new GraphLayout();
  const { nodes, edges } = fixture(200);
  layout.sync(nodes, edges);
  run(layout, 600);

  const kept = layout.bodies.get("item:i5");
  const position = { x: kept.x, y: kept.y };
  // Every tag, and the even-numbered saves — so i5 survives and i1 does not.
  const half = nodes.filter(
    (node) => !node.id.startsWith("item:i") || Number(node.id.slice(6)) % 2 === 1,
  );
  const keptIds = new Set(half.map((node) => node.id));
  layout.sync(
    half,
    edges.filter((edge) => keptIds.has(edge.source) && keptIds.has(edge.target)),
  );

  const survivor = layout.bodies.get("item:i5");
  assert.ok(survivor, "a surviving node must keep its body");
  assert.equal(survivor.x, position.x);
  assert.equal(survivor.y, position.y);
  assert.equal(layout.bodies.has("item:i2"), false, "a filtered node must leave");
  assert.equal(layout.settled, false, "a new node set must reheat");
});

test("seed positions are deterministic and spread with the population", () => {
  assert.deepEqual(seedPosition("item:a"), seedPosition("item:a"));

  const near = Math.hypot(...Object.values(seedPosition("item:a", 1)));
  const far = Math.hypot(...Object.values(seedPosition("item:a", 4)));
  // A thousand nodes must not be seeded into the same disc as fifty, or the
  // view opens on a dense knot and spends its first seconds exploding.
  assert.ok(far > near * 3.5, "the seed disc must grow with the node count");
});
