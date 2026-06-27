CREATE TABLE `chunks` (
	`chunk_hash` text PRIMARY KEY NOT NULL,
	`size_bytes` integer NOT NULL,
	`refcount` integer DEFAULT 0 NOT NULL,
	`created_at` integer DEFAULT (unixepoch() * 1000) NOT NULL
);
--> statement-breakpoint
CREATE TABLE `devices` (
	`device_id` text PRIMARY KEY NOT NULL,
	`user_id` text,
	`name` text NOT NULL,
	`token_hash` text NOT NULL,
	`created_at` integer DEFAULT (unixepoch() * 1000) NOT NULL
);
--> statement-breakpoint
CREATE TABLE `folder_grants` (
	`grant_id` text PRIMARY KEY NOT NULL,
	`folder_id` text NOT NULL,
	`scope_node_id` text NOT NULL,
	`user_id` text,
	`device_id` text,
	`role` text DEFAULT 'collaborator' NOT NULL,
	`can_read` integer DEFAULT true NOT NULL,
	`can_write` integer DEFAULT true NOT NULL,
	`created_at` integer DEFAULT (unixepoch() * 1000) NOT NULL,
	FOREIGN KEY (`folder_id`) REFERENCES `shared_folders`(`folder_id`) ON UPDATE no action ON DELETE no action,
	FOREIGN KEY (`scope_node_id`) REFERENCES `nodes`(`node_id`) ON UPDATE no action ON DELETE no action,
	FOREIGN KEY (`device_id`) REFERENCES `devices`(`device_id`) ON UPDATE no action ON DELETE no action,
	CONSTRAINT "folder_grants_principal_xor" CHECK(("folder_grants"."user_id" IS NULL) <> ("folder_grants"."device_id" IS NULL))
);
--> statement-breakpoint
CREATE INDEX `folder_grants_scope_principal_idx` ON `folder_grants` (`scope_node_id`,`folder_id`,`user_id`,`device_id`);--> statement-breakpoint
CREATE TABLE `folder_invites` (
	`invite_token` text PRIMARY KEY NOT NULL,
	`folder_id` text NOT NULL,
	`scope_node_id` text NOT NULL,
	`invited_email` text NOT NULL,
	`invited_by_user_id` text NOT NULL,
	`status` text DEFAULT 'pending' NOT NULL,
	`created_at` integer DEFAULT (unixepoch() * 1000) NOT NULL,
	`expires_at` integer NOT NULL,
	FOREIGN KEY (`folder_id`) REFERENCES `shared_folders`(`folder_id`) ON UPDATE no action ON DELETE no action,
	FOREIGN KEY (`scope_node_id`) REFERENCES `nodes`(`node_id`) ON UPDATE no action ON DELETE no action
);
--> statement-breakpoint
CREATE TABLE `nodes` (
	`node_id` text PRIMARY KEY NOT NULL,
	`folder_id` text NOT NULL,
	`parent_id` text,
	`name` text NOT NULL,
	`type` text NOT NULL,
	`current_version_id` text,
	`deleted_at` integer,
	`server_seq` integer DEFAULT 0 NOT NULL,
	FOREIGN KEY (`folder_id`) REFERENCES `shared_folders`(`folder_id`) ON UPDATE no action ON DELETE no action,
	FOREIGN KEY (`parent_id`) REFERENCES `nodes`(`node_id`) ON UPDATE no action ON DELETE no action,
	FOREIGN KEY (`current_version_id`) REFERENCES `versions`(`version_id`) ON UPDATE no action ON DELETE no action
);
--> statement-breakpoint
CREATE UNIQUE INDEX `nodes_live_name_unique` ON `nodes` (`folder_id`,`parent_id`,`name`) WHERE "nodes"."deleted_at" IS NULL;--> statement-breakpoint
CREATE INDEX `nodes_folder_parent_idx` ON `nodes` (`folder_id`,`parent_id`);--> statement-breakpoint
CREATE TABLE `op_log` (
	`server_seq` integer PRIMARY KEY AUTOINCREMENT NOT NULL,
	`folder_id` text NOT NULL,
	`node_id` text NOT NULL,
	`op_type` text NOT NULL,
	`op_payload` text NOT NULL,
	`based_on_seq` integer,
	`actor_device_id` text NOT NULL,
	`applied_at` integer DEFAULT (unixepoch() * 1000) NOT NULL,
	FOREIGN KEY (`folder_id`) REFERENCES `shared_folders`(`folder_id`) ON UPDATE no action ON DELETE no action,
	FOREIGN KEY (`actor_device_id`) REFERENCES `devices`(`device_id`) ON UPDATE no action ON DELETE no action
);
--> statement-breakpoint
CREATE INDEX `op_log_folder_seq_idx` ON `op_log` (`folder_id`,`server_seq`);--> statement-breakpoint
CREATE TABLE `shared_folders` (
	`folder_id` text PRIMARY KEY NOT NULL,
	`name` text NOT NULL,
	`owner_user_id` text NOT NULL,
	`created_at` integer DEFAULT (unixepoch() * 1000) NOT NULL
);
--> statement-breakpoint
CREATE TABLE `versions` (
	`version_id` text PRIMARY KEY NOT NULL,
	`node_id` text NOT NULL,
	`manifest` text NOT NULL,
	`content_hash` text NOT NULL,
	`size_bytes` integer NOT NULL,
	`author_device_id` text NOT NULL,
	`created_at` integer DEFAULT (unixepoch() * 1000) NOT NULL,
	`is_conflict_copy` integer DEFAULT false NOT NULL,
	FOREIGN KEY (`node_id`) REFERENCES `nodes`(`node_id`) ON UPDATE no action ON DELETE cascade,
	FOREIGN KEY (`author_device_id`) REFERENCES `devices`(`device_id`) ON UPDATE no action ON DELETE no action
);
--> statement-breakpoint
CREATE INDEX `versions_node_created_idx` ON `versions` (`node_id`,`created_at`);