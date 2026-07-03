import { AwsClient } from "aws4fetch";
import { v4 as uuidv4 } from "uuid";

export const testBucketEndpoint = "http://localhost:9000";

export function createTestS3Client(): AwsClient {
  return new AwsClient({
    region: "auto",
    service: "s3",
    accessKeyId: "minioadmin",
    secretAccessKey: "minioadmin",
  });
}

export async function createTestBucket(s3: AwsClient): Promise<string> {
  const bucket = `valv-e2e-${uuidv4()}`;
  await expectOk(s3.fetch(bucketUrl(bucket), { method: "PUT" }), `create bucket ${bucket}`);
  return bucket;
}

export async function deleteTestBucket(s3: AwsClient, bucket: string): Promise<void> {
  for (const key of await listKeys(s3, bucket)) {
    await expectOk(s3.fetch(bucketUrl(bucket, key), { method: "DELETE" }), `delete object ${key}`);
  }

  await expectOk(s3.fetch(bucketUrl(bucket), { method: "DELETE" }), `delete bucket ${bucket}`);
}

function bucketUrl(bucket: string, key = ""): string {
  const path = key ? `/${bucket}/${key}` : `/${bucket}`;
  return `${testBucketEndpoint}${path}`;
}

async function listKeys(s3: AwsClient, bucket: string): Promise<string[]> {
  const response = await expectOk(s3.fetch(`${bucketUrl(bucket)}?list-type=2`), `list bucket ${bucket}`);
  const xml = await response.text();
  return Array.from(xml.matchAll(/<Key>(.*?)<\/Key>/g), (match) => decodeXml(match[1] ?? ""));
}

async function expectOk(responsePromise: Promise<Response>, action: string): Promise<Response> {
  const response = await responsePromise;
  if (!response.ok) {
    throw new Error(`${action} failed: ${response.status} ${await response.text()}`);
  }
  return response;
}

function decodeXml(value: string): string {
  return value
    .replaceAll("&amp;", "&")
    .replaceAll("&lt;", "<")
    .replaceAll("&gt;", ">")
    .replaceAll("&quot;", '"')
    .replaceAll("&apos;", "'");
}
