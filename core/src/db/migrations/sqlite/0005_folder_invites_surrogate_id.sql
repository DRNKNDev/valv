PRAGMA foreign_keys=OFF;--> statement-breakpoint
CREATE TABLE `__new_folder_invites` (
	`invite_id` text NOT NULL,
	`invite_token` text PRIMARY KEY NOT NULL,
	`folder_id` text NOT NULL,
	`scope_node_id` text NOT NULL,
	`invited_email` text NOT NULL,
	`invited_by_user_id` text NOT NULL,
	`can_write` integer DEFAULT true NOT NULL,
	`status` text DEFAULT 'pending' NOT NULL,
	`created_at` integer DEFAULT (unixepoch() * 1000) NOT NULL,
	`expires_at` integer NOT NULL,
	FOREIGN KEY (`folder_id`) REFERENCES `shared_folders`(`folder_id`) ON UPDATE no action ON DELETE no action,
	FOREIGN KEY (`scope_node_id`) REFERENCES `nodes`(`node_id`) ON UPDATE no action ON DELETE no action
);
--> statement-breakpoint
INSERT INTO `__new_folder_invites`("invite_id", "invite_token", "folder_id", "scope_node_id", "invited_email", "invited_by_user_id", "can_write", "status", "created_at", "expires_at")
SELECT lower(hex(randomblob(16))), "invite_token", "folder_id", "scope_node_id", "invited_email", "invited_by_user_id", "can_write", "status", "created_at", "expires_at" FROM `folder_invites`;
--> statement-breakpoint
DROP TABLE `folder_invites`;
--> statement-breakpoint
ALTER TABLE `__new_folder_invites` RENAME TO `folder_invites`;
--> statement-breakpoint
CREATE UNIQUE INDEX `folder_invites_invite_id_unique` ON `folder_invites` (`invite_id`);
--> statement-breakpoint
PRAGMA foreign_keys=ON;
