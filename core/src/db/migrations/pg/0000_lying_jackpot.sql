CREATE TABLE "chunks" (
	"chunk_hash" text PRIMARY KEY NOT NULL,
	"size_bytes" integer NOT NULL,
	"refcount" integer DEFAULT 0 NOT NULL,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL
);
--> statement-breakpoint
CREATE TABLE "devices" (
	"device_id" uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL,
	"user_id" text,
	"name" text NOT NULL,
	"token_hash" text NOT NULL,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL
);
--> statement-breakpoint
CREATE TABLE "folder_grants" (
	"grant_id" uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL,
	"folder_id" uuid NOT NULL,
	"scope_node_id" uuid NOT NULL,
	"user_id" text,
	"device_id" uuid,
	"role" text DEFAULT 'collaborator' NOT NULL,
	"can_read" boolean DEFAULT true NOT NULL,
	"can_write" boolean DEFAULT true NOT NULL,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "folder_grants_principal_xor" CHECK (("folder_grants"."user_id" IS NULL) <> ("folder_grants"."device_id" IS NULL))
);
--> statement-breakpoint
CREATE TABLE "folder_invites" (
	"invite_token" text PRIMARY KEY NOT NULL,
	"folder_id" uuid NOT NULL,
	"scope_node_id" uuid NOT NULL,
	"invited_email" text NOT NULL,
	"invited_by_user_id" text NOT NULL,
	"status" text DEFAULT 'pending' NOT NULL,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	"expires_at" timestamp with time zone NOT NULL
);
--> statement-breakpoint
CREATE TABLE "nodes" (
	"node_id" uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL,
	"folder_id" uuid NOT NULL,
	"parent_id" uuid,
	"name" text NOT NULL,
	"type" text NOT NULL,
	"current_version_id" uuid,
	"deleted_at" timestamp with time zone,
	"server_seq" bigint DEFAULT 0 NOT NULL
);
--> statement-breakpoint
CREATE TABLE "op_log" (
	"server_seq" bigserial PRIMARY KEY NOT NULL,
	"folder_id" uuid NOT NULL,
	"node_id" uuid NOT NULL,
	"op_type" text NOT NULL,
	"op_payload" jsonb NOT NULL,
	"based_on_seq" bigint,
	"actor_device_id" uuid NOT NULL,
	"applied_at" timestamp with time zone DEFAULT now() NOT NULL
);
--> statement-breakpoint
CREATE TABLE "shared_folders" (
	"folder_id" uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL,
	"name" text NOT NULL,
	"owner_user_id" text NOT NULL,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL
);
--> statement-breakpoint
CREATE TABLE "versions" (
	"version_id" uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL,
	"node_id" uuid NOT NULL,
	"manifest" jsonb NOT NULL,
	"content_hash" text NOT NULL,
	"size_bytes" bigint NOT NULL,
	"author_device_id" uuid NOT NULL,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	"is_conflict_copy" boolean DEFAULT false NOT NULL
);
--> statement-breakpoint
ALTER TABLE "folder_grants" ADD CONSTRAINT "folder_grants_folder_id_shared_folders_folder_id_fk" FOREIGN KEY ("folder_id") REFERENCES "public"."shared_folders"("folder_id") ON DELETE no action ON UPDATE no action;--> statement-breakpoint
ALTER TABLE "folder_grants" ADD CONSTRAINT "folder_grants_scope_node_id_nodes_node_id_fk" FOREIGN KEY ("scope_node_id") REFERENCES "public"."nodes"("node_id") ON DELETE no action ON UPDATE no action;--> statement-breakpoint
ALTER TABLE "folder_grants" ADD CONSTRAINT "folder_grants_device_id_devices_device_id_fk" FOREIGN KEY ("device_id") REFERENCES "public"."devices"("device_id") ON DELETE no action ON UPDATE no action;--> statement-breakpoint
ALTER TABLE "folder_invites" ADD CONSTRAINT "folder_invites_folder_id_shared_folders_folder_id_fk" FOREIGN KEY ("folder_id") REFERENCES "public"."shared_folders"("folder_id") ON DELETE no action ON UPDATE no action;--> statement-breakpoint
ALTER TABLE "folder_invites" ADD CONSTRAINT "folder_invites_scope_node_id_nodes_node_id_fk" FOREIGN KEY ("scope_node_id") REFERENCES "public"."nodes"("node_id") ON DELETE no action ON UPDATE no action;--> statement-breakpoint
ALTER TABLE "nodes" ADD CONSTRAINT "nodes_folder_id_shared_folders_folder_id_fk" FOREIGN KEY ("folder_id") REFERENCES "public"."shared_folders"("folder_id") ON DELETE no action ON UPDATE no action;--> statement-breakpoint
ALTER TABLE "nodes" ADD CONSTRAINT "nodes_parent_id_nodes_node_id_fk" FOREIGN KEY ("parent_id") REFERENCES "public"."nodes"("node_id") ON DELETE no action ON UPDATE no action;--> statement-breakpoint
ALTER TABLE "nodes" ADD CONSTRAINT "nodes_current_version_id_versions_version_id_fk" FOREIGN KEY ("current_version_id") REFERENCES "public"."versions"("version_id") ON DELETE no action ON UPDATE no action;--> statement-breakpoint
ALTER TABLE "op_log" ADD CONSTRAINT "op_log_folder_id_shared_folders_folder_id_fk" FOREIGN KEY ("folder_id") REFERENCES "public"."shared_folders"("folder_id") ON DELETE no action ON UPDATE no action;--> statement-breakpoint
ALTER TABLE "op_log" ADD CONSTRAINT "op_log_actor_device_id_devices_device_id_fk" FOREIGN KEY ("actor_device_id") REFERENCES "public"."devices"("device_id") ON DELETE no action ON UPDATE no action;--> statement-breakpoint
ALTER TABLE "versions" ADD CONSTRAINT "versions_node_id_nodes_node_id_fk" FOREIGN KEY ("node_id") REFERENCES "public"."nodes"("node_id") ON DELETE cascade ON UPDATE no action;--> statement-breakpoint
ALTER TABLE "versions" ADD CONSTRAINT "versions_author_device_id_devices_device_id_fk" FOREIGN KEY ("author_device_id") REFERENCES "public"."devices"("device_id") ON DELETE no action ON UPDATE no action;--> statement-breakpoint
CREATE INDEX "folder_grants_scope_principal_idx" ON "folder_grants" USING btree ("scope_node_id","folder_id","user_id","device_id");--> statement-breakpoint
CREATE UNIQUE INDEX "nodes_live_name_unique" ON "nodes" USING btree ("folder_id","parent_id","name") WHERE "nodes"."deleted_at" IS NULL;--> statement-breakpoint
CREATE INDEX "nodes_folder_parent_idx" ON "nodes" USING btree ("folder_id","parent_id");--> statement-breakpoint
CREATE INDEX "op_log_folder_seq_idx" ON "op_log" USING btree ("folder_id","server_seq");--> statement-breakpoint
CREATE INDEX "versions_node_created_idx" ON "versions" USING btree ("node_id","created_at");