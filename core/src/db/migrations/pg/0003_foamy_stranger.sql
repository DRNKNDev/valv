CREATE TABLE "version_chunks" (
	"version_id" uuid NOT NULL,
	"node_id" uuid NOT NULL,
	"chunk_hash" text NOT NULL,
	CONSTRAINT "version_chunks_version_id_chunk_hash_pk" PRIMARY KEY("version_id","chunk_hash")
);
--> statement-breakpoint
ALTER TABLE "version_chunks" ADD CONSTRAINT "version_chunks_version_id_versions_version_id_fk" FOREIGN KEY ("version_id") REFERENCES "public"."versions"("version_id") ON DELETE cascade ON UPDATE no action;--> statement-breakpoint
ALTER TABLE "version_chunks" ADD CONSTRAINT "version_chunks_node_id_nodes_node_id_fk" FOREIGN KEY ("node_id") REFERENCES "public"."nodes"("node_id") ON DELETE no action ON UPDATE no action;--> statement-breakpoint
ALTER TABLE "version_chunks" ADD CONSTRAINT "version_chunks_chunk_hash_chunks_chunk_hash_fk" FOREIGN KEY ("chunk_hash") REFERENCES "public"."chunks"("chunk_hash") ON DELETE no action ON UPDATE no action;--> statement-breakpoint
CREATE INDEX "version_chunks_chunk_hash_idx" ON "version_chunks" USING btree ("chunk_hash");