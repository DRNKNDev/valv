#!/usr/bin/env node

const crypto = require("node:crypto");
const http = require("node:http");

const bucket = process.env.SPIKE_BUCKET;
const endpoint = process.env.SPIKE_ENDPOINT;
const accessKeyId = process.env.AWS_ACCESS_KEY_ID;
const secretAccessKey = process.env.AWS_SECRET_ACCESS_KEY;
const region = process.env.AWS_DEFAULT_REGION || "auto";
const port = Number(process.env.SPIKE_HELPER_PORT || 8787);
const expiresIn = Number(process.env.SPIKE_PRESIGN_TTL || 3600);

if (!bucket || !endpoint || !accessKeyId || !secretAccessKey) {
  console.error("Missing required environment variables.");
  console.error("Required: SPIKE_BUCKET, SPIKE_ENDPOINT, AWS_ACCESS_KEY_ID, AWS_SECRET_ACCESS_KEY");
  process.exit(1);
}

const endpointURL = new URL(endpoint);
const service = "s3";

function sha256(value) {
  return crypto.createHash("sha256").update(value).digest("hex");
}

function hmac(key, value, encoding) {
  return crypto.createHmac("sha256", key).update(value).digest(encoding);
}

function getSigningKey(dateStamp) {
  const kDate = hmac(`AWS4${secretAccessKey}`, dateStamp);
  const kRegion = hmac(kDate, region);
  const kService = hmac(kRegion, service);
  return hmac(kService, "aws4_request");
}

function awsTimestamp(date = new Date()) {
  return date.toISOString().replace(/[:-]|\.\d{3}/g, "");
}

function dateStamp(date = new Date()) {
  return date.toISOString().slice(0, 10).replace(/-/g, "");
}

function encodeRFC3986(value) {
  return encodeURIComponent(value).replace(/[!'()*]/g, (char) => `%${char.charCodeAt(0).toString(16).toUpperCase()}`);
}

function encodeKeyPath(key) {
  return key.split("/").map(encodeRFC3986).join("/");
}

function bucketPath(key = "") {
  const base = endpointURL.pathname.replace(/\/$/, "");
  const bucketSegment = encodeRFC3986(bucket);
  const keySegment = key ? `/${encodeKeyPath(key)}` : "";
  return `${base}/${bucketSegment}${keySegment}`.replace(/\/\/+/, "/");
}

function canonicalQuery(query) {
  return [...query.entries()]
    .sort(([left], [right]) => left.localeCompare(right))
    .map(([name, value]) => `${encodeRFC3986(name)}=${encodeRFC3986(value)}`)
    .join("&");
}

function signRequest(method, path, query) {
  const now = new Date();
  const amzDate = awsTimestamp(now);
  const shortDate = dateStamp(now);
  const canonicalHeaders = `host:${endpointURL.host}\nx-amz-content-sha256:${sha256("")}\nx-amz-date:${amzDate}\n`;
  const signedHeaders = "host;x-amz-content-sha256;x-amz-date";
  const payloadHash = sha256("");
  const queryString = canonicalQuery(query);
  const canonicalRequest = [method, path, queryString, canonicalHeaders, signedHeaders, payloadHash].join("\n");
  const credentialScope = `${shortDate}/${region}/${service}/aws4_request`;
  const stringToSign = ["AWS4-HMAC-SHA256", amzDate, credentialScope, sha256(canonicalRequest)].join("\n");
  const signature = hmac(getSigningKey(shortDate), stringToSign, "hex");

  return {
    headers: {
      host: endpointURL.host,
      "x-amz-content-sha256": payloadHash,
      "x-amz-date": amzDate,
      authorization: `AWS4-HMAC-SHA256 Credential=${accessKeyId}/${credentialScope}, SignedHeaders=${signedHeaders}, Signature=${signature}`
    },
    queryString
  };
}

function presignURL(method, key) {
  const path = bucketPath(key);
  const now = new Date();
  const amzDate = awsTimestamp(now);
  const shortDate = dateStamp(now);
  const credentialScope = `${shortDate}/${region}/${service}/aws4_request`;
  const query = new URLSearchParams({
    "X-Amz-Algorithm": "AWS4-HMAC-SHA256",
    "X-Amz-Credential": `${accessKeyId}/${credentialScope}`,
    "X-Amz-Date": amzDate,
    "X-Amz-Expires": String(expiresIn),
    "X-Amz-SignedHeaders": "host"
  });

  const canonicalRequest = [
    method,
    path,
    canonicalQuery(query),
    `host:${endpointURL.host}\n`,
    "host",
    "UNSIGNED-PAYLOAD"
  ].join("\n");
  const stringToSign = ["AWS4-HMAC-SHA256", amzDate, credentialScope, sha256(canonicalRequest)].join("\n");
  const signature = hmac(getSigningKey(shortDate), stringToSign, "hex");
  query.set("X-Amz-Signature", signature);

  return `${endpointURL.origin}${path}?${canonicalQuery(query)}`;
}

function decodeXml(value) {
  return value
    .replace(/&amp;/g, "&")
    .replace(/&lt;/g, "<")
    .replace(/&gt;/g, ">")
    .replace(/&quot;/g, '"')
    .replace(/&#39;/g, "'");
}

function parseListObjects(xml) {
  const objects = [];
  const contents = [...xml.matchAll(/<Contents>([\s\S]*?)<\/Contents>/g)];

  for (const [, block] of contents) {
    const keyMatch = block.match(/<Key>([\s\S]*?)<\/Key>/);
    const sizeMatch = block.match(/<Size>(\d+)<\/Size>/);

    if (!keyMatch) {
      continue;
    }

    objects.push({
      key: decodeXml(keyMatch[1]),
      size: sizeMatch ? Number(sizeMatch[1]) : null,
      contentType: null
    });
  }

  const nextTokenMatch = xml.match(/<NextContinuationToken>([\s\S]*?)<\/NextContinuationToken>/);
  return {
    objects,
    nextToken: nextTokenMatch ? decodeXml(nextTokenMatch[1]) : null
  };
}

async function listObjects() {
  const items = [];
  let continuationToken = null;

  while (true) {
    const query = new URLSearchParams({ "list-type": "2", "max-keys": "1000" });
    if (continuationToken) {
      query.set("continuation-token", continuationToken);
    }

    const path = bucketPath();
    const { headers, queryString } = signRequest("GET", path, query);
    const response = await fetch(`${endpointURL.origin}${path}?${queryString}`, { headers });

    if (!response.ok) {
      const body = await response.text();
      throw new Error(`ListObjectsV2 failed with HTTP ${response.status}: ${body}`);
    }

    const xml = await response.text();
    const page = parseListObjects(xml);
    items.push(...page.objects);

    if (!page.nextToken) {
      break;
    }

    continuationToken = page.nextToken;
  }

  return items;
}

function writeJson(response, statusCode, value) {
  response.writeHead(statusCode, { "content-type": "application/json" });
  response.end(JSON.stringify(value));
}

function notFound(response) {
  writeJson(response, 404, { error: "Not found" });
}

const server = http.createServer(async (request, response) => {
  try {
    const url = new URL(request.url, `http://127.0.0.1:${port}`);

    if (request.method === "GET" && url.pathname === "/health") {
      writeJson(response, 200, { ok: true, bucket, endpoint: endpointURL.origin });
      return;
    }

    if (request.method === "GET" && url.pathname === "/objects") {
      writeJson(response, 200, await listObjects());
      return;
    }

    if (request.method === "GET" && url.pathname === "/presign/download") {
      const key = url.searchParams.get("key");
      if (!key) {
        writeJson(response, 400, { error: "Missing key query parameter" });
        return;
      }

      writeJson(response, 200, { url: presignURL("GET", key) });
      return;
    }

    if (request.method === "GET" && url.pathname === "/presign/upload") {
      const key = url.searchParams.get("key");
      if (!key) {
        writeJson(response, 400, { error: "Missing key query parameter" });
        return;
      }

      writeJson(response, 200, { url: presignURL("PUT", key) });
      return;
    }

    if (request.method === "GET" && url.pathname === "/presign/delete") {
      const key = url.searchParams.get("key");
      if (!key) {
        writeJson(response, 400, { error: "Missing key query parameter" });
        return;
      }

      writeJson(response, 200, { url: presignURL("DELETE", key) });
      return;
    }

    notFound(response);
  } catch (error) {
    writeJson(response, 500, { error: error.message });
  }
});

server.listen(port, "127.0.0.1", () => {
  console.log(`Spike presign helper listening on http://127.0.0.1:${port}`);
});
