import {
  CreateBucketCommand,
  DeleteBucketCommand,
  DeleteObjectsCommand,
  ListObjectsV2Command,
  S3Client,
} from "@aws-sdk/client-s3";
import { v4 as uuidv4 } from "uuid";

export function createTestS3Client(): S3Client {
  return new S3Client({
    endpoint: "http://localhost:9000",
    forcePathStyle: true,
    region: "auto",
    credentials: {
      accessKeyId: "minioadmin",
      secretAccessKey: "minioadmin",
    },
  });
}

export async function createTestBucket(s3: S3Client): Promise<string> {
  const bucket = `valv-e2e-${uuidv4()}`;
  await s3.send(new CreateBucketCommand({ Bucket: bucket }));
  return bucket;
}

export async function deleteTestBucket(s3: S3Client, bucket: string): Promise<void> {
  let token: string | undefined;
  do {
    const listed = await s3.send(new ListObjectsV2Command({ Bucket: bucket, ContinuationToken: token }));
    const objects = listed.Contents?.map((object: { Key?: string }) => (object.Key ? { Key: object.Key } : undefined)).filter(
      (object: { Key: string } | undefined): object is { Key: string } => object !== undefined,
    );
    if (objects && objects.length > 0) {
      await s3.send(new DeleteObjectsCommand({ Bucket: bucket, Delete: { Objects: objects } }));
    }
    token = listed.NextContinuationToken;
  } while (token);

  await s3.send(new DeleteBucketCommand({ Bucket: bucket }));
}
