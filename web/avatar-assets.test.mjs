import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { build } from "esbuild";
import { JSDOM } from "jsdom";

const web = path.dirname(fileURLToPath(import.meta.url));
const version = "clay-20260828-v1";

async function loadModule(name) {
  const built = await build({ entryPoints: [path.join(web, name)], bundle: true,
    write: false, format: "esm", logLevel: "silent", plugins: [{ name: "served-modules", setup(builder) {
      builder.onResolve({ filter: /^\// }, ({ path: url, kind }) =>
        kind === "entry-point" ? undefined : { path: path.join(web, url) });
    } }] });
  return import(`data:text/javascript;base64,${Buffer.from(built.outputFiles[0].text).toString("base64")}`);
}

test("original delivery assets retain alpha and sufficient Retina resolution", async () => {
  for (const [name, size] of [["symbiont-avatar-display.png", 192],
    ["symbiont-avatar-small.png", 96], ["input-role-avatars/symbiont-dissent-small.png", 128]]) {
    const png = await readFile(path.join(web, "assets", name));
    assert.equal(png.subarray(1, 4).toString(), "PNG");
    assert.equal(png.readUInt32BE(16), size, name);
    assert.equal(png.readUInt32BE(20), size, name);
    assert.equal(png[25], 6, `${name} must keep RGBA transparency`);
    assert.ok(png.length < 100_000, `${name} must remain a small delivery asset`);
  }
});

test("normal avatar cache version agrees across initial, topic, preview and dynamic rendering", async () => {
  const html = await readFile(path.join(web, "index.html"), "utf8");
  const dom = new JSDOM(html, { url: "http://symbiont.test" });
  const previous = globalThis.document;
  try {
    globalThis.document = dom.window.document;
    const { initIdentityUi } = await loadModule("identity-ui.js");
    const state = { identity: {} };
    const identity = initIdentityUi(state);
    identity.render();
    assert.equal(document.querySelector("#avatar-preview").getAttribute("src"), `/symbiont-avatar.png?v=${version}`);
    assert.ok(document.querySelector("#avatar-preview").classList.contains("symbiont-avatar-art"));
    const container = document.createElement("div");
    container.className = "message-avatar";
    container.innerHTML = '<img class="message-avatar-image">';
    identity.applyAvatar(container, "symbiont");
    assert.equal(container.firstElementChild.getAttribute("src"), `/symbiont-avatar-small.png?v=${version}`);
    assert.ok(container.firstElementChild.classList.contains("symbiont-avatar-art"));
    const topic = await readFile(path.join(web, "topic-ui.js"), "utf8");
    assert.ok(topic.includes(`/symbiont-avatar-small.png?v=${version}`));
    assert.ok(html.includes(`/symbiont-avatar-small.png?v=${version}`));
    assert.ok(html.includes(`/symbiont-avatar.png?v=${version}`));
    state.identity.avatar = { url: "/api/assets/custom.png" };
    identity.applyAvatar(container, "symbiont");
    assert.equal(container.firstElementChild.getAttribute("src"), "/api/assets/custom.png");
    assert.ok(!container.firstElementChild.classList.contains("symbiont-avatar-art"));
    container.firstElementChild.onerror();
    assert.equal(container.firstElementChild.getAttribute("src"), `/symbiont-avatar-small.png?v=${version}`);
    assert.ok(container.firstElementChild.classList.contains("symbiont-avatar-art"));
    state.identity.userAvatar = { url: "/api/assets/user.png" };
    identity.applyAvatar(container, "user");
    assert.ok(!container.firstElementChild.classList.contains("symbiont-avatar-art"));
  } finally {
    globalThis.document = previous;
    dom.window.close();
  }
});

test("only dissent receives display treatment; other identities and image reuse remain intact", async () => {
  const dom = new JSDOM('<div id="avatar"></div>');
  const previous = globalThis.document;
  try {
    globalThis.document = dom.window.document;
    const { applyInputRoleAvatar } = await loadModule("input-roles.js");
    const container = document.querySelector("#avatar");
    applyInputRoleAvatar(container, "symbiont-dissent");
    const image = container.firstElementChild;
    assert.equal(image.getAttribute("src"), `/assets/input-role-avatars/symbiont-dissent.png?v=${version}`);
    assert.ok(image.classList.contains("symbiont-avatar-art"));
    applyInputRoleAvatar(container, "moon-window");
    assert.equal(container.firstElementChild, image);
    assert.equal(image.getAttribute("src"), "/assets/input-role-avatars/moon-window.png?v=clay-20260828-v1");
    assert.ok(!image.classList.contains("symbiont-avatar-art"));
    applyInputRoleAvatar(container, "symbiont-dissent");
    assert.equal(container.childElementCount, 1);
    assert.equal(container.dataset.inputAvatar, "symbiont-dissent");
    assert.ok(image.classList.contains("symbiont-avatar-art"));
  } finally {
    globalThis.document = previous;
    dom.window.close();
  }
});

test("shared midtone curve preserves black, white and transparency without changing the art", async () => {
  const dom = new JSDOM(await readFile(path.join(web, "index.html"), "utf8"));
  try {
    const filter = dom.window.document.querySelector("#symbiont-avatar-midtone");
    assert.equal(filter.getAttribute("color-interpolation-filters"), "sRGB");
    const channels = [...filter.querySelector("feComponentTransfer").children];
    assert.equal(channels.length, 3, "alpha must not be adjusted");
    const curve = channels[0].getAttribute("tableValues");
    for (const channel of channels) assert.equal(channel.getAttribute("tableValues"), curve);
    const values = curve.split(/\s+/).map(Number);
    assert.equal(values[0], 0);
    assert.equal(values.at(-1), 1);
    assert.ok(values[3] > .3 && values[3] <= .36);
    assert.ok(values[9] <= .92, "pale highlights must not be washed out");
    for (let i = 1; i < values.length; i++) assert.ok(values[i] > values[i - 1]);
  } finally {
    dom.window.close();
  }
});
