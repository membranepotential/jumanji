// Unit test for worker.js under plain node, no install:
//   node --test deploy/cloudflare/
// `crypto.subtle.timingSafeEqual` is a Workers extension; node's own
// `crypto.timingSafeEqual` stands in for it.
import assert from "node:assert/strict";
import { timingSafeEqual } from "node:crypto";
import { test } from "node:test";

Object.defineProperty(crypto.subtle, "timingSafeEqual", {
  value: (a, b) => timingSafeEqual(new Uint8Array(a), new Uint8Array(b)),
});
const { default: worker, authorized } = await import("./worker.js");

const basic = (credentials) =>
  new Request("https://site.example/", {
    headers: {
      Authorization: `Basic ${Buffer.from(credentials).toString("base64")}`,
    },
  });

const env = (password) => ({
  SITE_PASSWORD: password,
  ASSETS: {
    fetch: async () =>
      new Response("<p>page</p>", {
        headers: { "Cache-Control": "public, max-age=0, must-revalidate" },
      }),
  },
});

test("the password opens the site, whatever the user name", async () => {
  assert.equal(await authorized(basic("anyone:s3cret ü"), "s3cret ü"), true);
  assert.equal(await authorized(basic(":s3cret ü"), "s3cret ü"), true);
  assert.equal(await authorized(basic("a:b:c"), "b:c"), true);
});

test("anything else is refused", async () => {
  assert.equal(await authorized(basic("me:wrong"), "s3cret"), false);
  assert.equal(await authorized(basic("me:s3cret"), "s3cre"), false);
  assert.equal(await authorized(basic("no colon"), "s3cret"), false);
  assert.equal(await authorized(new Request("https://site.example/"), "s3cret"), false);
  const bearer = new Request("https://site.example/", {
    headers: { Authorization: "Bearer s3cret" },
  });
  assert.equal(await authorized(bearer, "s3cret"), false);
});

test("an unset secret fails closed", async () => {
  assert.equal(await authorized(basic("me:"), undefined), false);
  assert.equal(await authorized(basic("me:"), ""), false);
});

test("a refusal asks for a password and stays unindexed", async () => {
  const response = await worker.fetch(basic("me:wrong"), env("s3cret"));
  assert.equal(response.status, 401);
  assert.equal(
    response.headers.get("WWW-Authenticate"),
    'Basic realm="jumanji", charset="UTF-8"',
  );
  assert.equal(response.headers.get("X-Robots-Tag"), "noindex, nofollow");
});

test("a served page is unindexed and private", async () => {
  const response = await worker.fetch(basic("me:s3cret"), env("s3cret"));
  assert.equal(response.status, 200);
  assert.equal(await response.text(), "<p>page</p>");
  assert.equal(response.headers.get("X-Robots-Tag"), "noindex, nofollow");
  assert.match(response.headers.get("Cache-Control"), /^private/);
});
