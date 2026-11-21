import test from "node:test";
import assert from "node:assert/strict";
import { extractWebAppDomain, normalizeWebUrl } from "../src/utils/webApps.ts";

test("normalizeWebUrl enforces http/https and adds https prefix for bare domains", () => {
  assert.equal(normalizeWebUrl("notion.so"), "https://notion.so/");
  assert.equal(normalizeWebUrl("https://app.slack.com/client"), "https://app.slack.com/client");
  assert.equal(normalizeWebUrl("http://localhost:3000/dashboard"), "http://localhost:3000/dashboard");
  assert.equal(normalizeWebUrl("javascript:alert(1)"), "");
  assert.equal(normalizeWebUrl("file:///etc/passwd"), "");
  assert.equal(normalizeWebUrl("not a url !!"), "");
});

test("extractWebAppDomain parses host cleanly", () => {
  assert.equal(extractWebAppDomain("https://calendar.google.com/calendar/u/0/r"), "calendar.google.com");
});
