/**
 * Teza Zero-Secret Google Drive OAuth Token Broker (Cloudflare Worker)
 *
 * Security invariants:
 * - Holds GOOGLE_CLIENT_SECRET strictly server-side so desktop binaries embed ZERO secrets.
 * - Accepts ONLY POST /token (PKCE code exchange) and POST /refresh (token refresh), plus GET /health.
 * - Enforces exact client_id match, RFC 7636 PKCE verifier format, and strictly loopback
 *   redirect_uri (`http://127.0.0.1:<port>/callback`) to prevent open-redirect token theft.
 * - Stateless: NEVER stores or logs tokens, NEVER accesses Google Drive files or E2EE keys.
 */

export interface Env {
  GOOGLE_CLIENT_ID: string;
  GOOGLE_CLIENT_SECRET: string;
}

const GOOGLE_TOKEN_ENDPOINT = "https://oauth2.googleapis.com/token";
const MAX_JSON_BODY_BYTES = 8 * 1024;
const LOOPBACK_REDIRECT_RE = /^http:\/\/127\.0\.0\.1:([1-9]\d{0,4})\/callback$/;
const PKCE_VERIFIER_RE = /^[A-Za-z0-9\-._~]{43,128}$/;

function jsonResponse(payload: unknown, status = 200): Response {
  return new Response(JSON.stringify(payload), {
    status,
    headers: {
      "Content-Type": "application/json; charset=utf-8",
      "Cache-Control": "no-store",
      "X-Content-Type-Options": "nosniff",
      "Referrer-Policy": "no-referrer",
    },
  });
}

function constantTimeEquals(a: string, b: string): boolean {
  const enc = new TextEncoder();
  const ba = enc.encode(a);
  const bb = enc.encode(b);
  let diff = ba.length ^ bb.length;
  const maxLen = Math.max(ba.length, bb.length);
  for (let i = 0; i < maxLen; i++) {
    diff |= (ba[i] ?? 0) ^ (bb[i] ?? 0);
  }
  return diff === 0;
}

async function readBoundedJson(req: Request): Promise<Record<string, unknown> | null> {
  const contentLength = Number(req.headers.get("content-length") ?? "0");
  if (Number.isFinite(contentLength) && contentLength > MAX_JSON_BODY_BYTES) {
    return null;
  }
  const buf = await req.arrayBuffer();
  if (buf.byteLength === 0 || buf.byteLength > MAX_JSON_BODY_BYTES) {
    return null;
  }
  try {
    const parsed: unknown = JSON.parse(new TextDecoder().decode(buf));
    if (parsed && typeof parsed === "object" && !Array.isArray(parsed)) {
      return parsed as Record<string, unknown>;
    }
    return null;
  } catch {
    return null;
  }
}

async function forwardToGoogleTokenEndpoint(params: URLSearchParams): Promise<Response> {
  const upstream = await fetch(GOOGLE_TOKEN_ENDPOINT, {
    method: "POST",
    headers: {
      "Content-Type": "application/x-www-form-urlencoded",
      Accept: "application/json",
    },
    body: params.toString(),
  });
  const body = await upstream.text();
  return new Response(body, {
    status: upstream.status,
    headers: {
      "Content-Type": "application/json; charset=utf-8",
      "Cache-Control": "no-store",
      "X-Content-Type-Options": "nosniff",
      "Referrer-Policy": "no-referrer",
    },
  });
}

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    const url = new URL(request.url);
    const path = url.pathname.replace(/\/+$/, "") || "/";

    if (request.method === "GET" && path === "/health") {
      return jsonResponse({
        ok: true,
        configured: Boolean(env.GOOGLE_CLIENT_ID && env.GOOGLE_CLIENT_SECRET),
      });
    }

    if (request.method !== "POST") {
      return jsonResponse({ error: "method_not_allowed" }, 405);
    }

    if (!env.GOOGLE_CLIENT_ID || !env.GOOGLE_CLIENT_SECRET) {
      return jsonResponse({ error: "broker_not_configured" }, 500);
    }

    const body = await readBoundedJson(request);
    if (!body) {
      return jsonResponse({ error: "invalid_json_body" }, 400);
    }

    const clientId = typeof body.client_id === "string" ? body.client_id.trim() : "";
    if (!clientId || !constantTimeEquals(clientId, env.GOOGLE_CLIENT_ID.trim())) {
      return jsonResponse({ error: "invalid_client_id" }, 403);
    }

    if (path === "/token") {
      const code = typeof body.code === "string" ? body.code.trim() : "";
      const codeVerifier = typeof body.code_verifier === "string" ? body.code_verifier.trim() : "";
      const redirectUri = typeof body.redirect_uri === "string" ? body.redirect_uri.trim() : "";

      if (!code || code.length > 2048) {
        return jsonResponse({ error: "invalid_code" }, 400);
      }
      if (!PKCE_VERIFIER_RE.test(codeVerifier)) {
        return jsonResponse({ error: "invalid_code_verifier" }, 400);
      }
      const match = LOOPBACK_REDIRECT_RE.exec(redirectUri);
      const port = match ? Number(match[1]) : 0;
      if (!match || port < 1024 || port > 65535) {
        return jsonResponse({ error: "invalid_redirect_uri" }, 400);
      }

      const form = new URLSearchParams({
        grant_type: "authorization_code",
        code,
        client_id: env.GOOGLE_CLIENT_ID.trim(),
        client_secret: env.GOOGLE_CLIENT_SECRET.trim(),
        code_verifier: codeVerifier,
        redirect_uri: redirectUri,
      });
      return forwardToGoogleTokenEndpoint(form);
    }

    if (path === "/refresh") {
      const refreshToken = typeof body.refresh_token === "string" ? body.refresh_token.trim() : "";
      if (!refreshToken || refreshToken.length > 2048) {
        return jsonResponse({ error: "invalid_refresh_token" }, 400);
      }

      const form = new URLSearchParams({
        grant_type: "refresh_token",
        refresh_token: refreshToken,
        client_id: env.GOOGLE_CLIENT_ID.trim(),
        client_secret: env.GOOGLE_CLIENT_SECRET.trim(),
      });
      return forwardToGoogleTokenEndpoint(form);
    }

    return jsonResponse({ error: "not_found" }, 404);
  },
};
