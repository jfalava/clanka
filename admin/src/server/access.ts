// Cloudflare Access stands in front of clanka.jfa.dev/admin and signs every request it lets
// through with a JWT in `Cf-Access-Jwt-Assertion`. The Worker checks that signature against the
// team's published keys before it reads anything, so a request that reaches it some other way
// (a misrouted binding, an Access app removed by mistake) gets no data.
import { Clock, Effect, Option, Schema } from "effect";

const SKEW_MS = 60 * 1000;
const KEYS_TTL_MS = 5 * 60 * 1000;

const Jwk = Schema.Struct({
  kid: Schema.String,
  kty: Schema.String,
  alg: Schema.optional(Schema.String),
  n: Schema.optional(Schema.String),
  e: Schema.optional(Schema.String),
});
type Jwk = typeof Jwk.Type;
const Certs = Schema.Struct({ keys: Schema.Array(Jwk) });

const Header = Schema.Struct({ alg: Schema.Literal("RS256"), kid: Schema.String });
// Access sends `aud` as a list, even for one application.
const Payload = Schema.Struct({
  aud: Schema.Union([Schema.String, Schema.Array(Schema.String)]),
  iss: Schema.String,
  exp: Schema.Finite,
  nbf: Schema.optional(Schema.Finite),
  email: Schema.optional(Schema.String),
});

const base64url = (text: string) => {
  const binary = atob(text.replace(/-/g, "+").replace(/_/g, "/").padEnd(Math.ceil(text.length / 4) * 4, "="));
  return Uint8Array.from(binary, (char) => char.charCodeAt(0));
};

const text = (segment: string) => new TextDecoder().decode(base64url(segment));
const decodeHeader = Schema.decodeUnknownOption(Schema.fromJsonString(Header));
const decodePayload = Schema.decodeUnknownOption(Schema.fromJsonString(Payload));

let cached: { issuer: string; keys: ReadonlyArray<Jwk>; until: number } | undefined;

const keys = (issuer: string, refresh: boolean) =>
  Effect.gen(function* () {
    const now = yield* Clock.currentTimeMillis;
    if (!refresh && cached?.issuer === issuer && cached.until > now) return cached.keys;
    const response = yield* Effect.tryPromise(() => fetch(`${issuer}/cdn-cgi/access/certs`));
    const certs = yield* Effect.tryPromise(() => response.json()).pipe(
      Effect.flatMap(Schema.decodeUnknownEffect(Certs)),
    );
    cached = { issuer, keys: certs.keys, until: now + KEYS_TTL_MS };
    return certs.keys;
  });

const signedBy = (jwk: Jwk, signature: Uint8Array<ArrayBuffer>, content: Uint8Array<ArrayBuffer>) =>
  Effect.tryPromise(async () => {
    const key = await crypto.subtle.importKey(
      "jwk",
      jwk,
      { name: "RSASSA-PKCS1-v1_5", hash: "SHA-256" },
      false,
      ["verify"],
    );
    return crypto.subtle.verify("RSASSA-PKCS1-v1_5", key, signature, content);
  }).pipe(Effect.orElseSucceed(() => false));

/**
 * Whether `assertion` is an Access JWT for the application `aud` of team `teamDomain`
 * (`<team>.cloudflareaccess.com`): signed by one of its keys, for this application, in date.
 * Fails only when the keys can't be fetched.
 */
export const verifyAccess = (assertion: string, teamDomain: string, aud: string) =>
  Effect.gen(function* () {
    const [header, payload, signature, ...rest] = assertion.split(".");
    if (!header || !payload || !signature || rest.length > 0) return false;
    const decodedHeader = yield* Effect.try(() => decodeHeader(text(header))).pipe(
      Effect.orElseSucceed(() => Option.none()),
    );
    const decodedPayload = yield* Effect.try(() => decodePayload(text(payload))).pipe(
      Effect.orElseSucceed(() => Option.none()),
    );
    if (Option.isNone(decodedHeader) || Option.isNone(decodedPayload)) return false;
    const { kid } = decodedHeader.value;
    const claims = decodedPayload.value;

    const issuer = `https://${teamDomain}`;
    const now = yield* Clock.currentTimeMillis;
    const audiences = typeof claims.aud === "string" ? [claims.aud] : claims.aud;
    if (claims.iss !== issuer || !audiences.includes(aud)) return false;
    if (claims.exp * 1000 + SKEW_MS <= now) return false;
    if (claims.nbf !== undefined && claims.nbf * 1000 > now + SKEW_MS) return false;

    // A key Access rotated in since the last fetch is fetched again, once.
    let jwk = (yield* keys(issuer, false)).find((key) => key.kid === kid);
    jwk ??= (yield* keys(issuer, true)).find((key) => key.kid === kid);
    if (!jwk || jwk.kty !== "RSA") return false;
    return yield* signedBy(
      jwk,
      base64url(signature),
      new TextEncoder().encode(`${header}.${payload}`),
    );
  });
