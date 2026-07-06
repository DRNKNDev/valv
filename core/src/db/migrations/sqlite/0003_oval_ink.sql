CREATE TABLE `version_chunks` (
	`version_id` text NOT NULL,
	`node_id` text NOT NULL,
	`chunk_hash` text NOT NULL,
	PRIMARY KEY(`version_id`, `chunk_hash`),
	FOREIGN KEY (`version_id`) REFERENCES `versions`(`version_id`) ON UPDATE no action ON DELETE cascade,
	FOREIGN KEY (`node_id`) REFERENCES `nodes`(`node_id`) ON UPDATE no action ON DELETE no action,
	FOREIGN KEY (`chunk_hash`) REFERENCES `chunks`(`chunk_hash`) ON UPDATE no action ON DELETE no action
);
--> statement-breakpoint
CREATE INDEX `version_chunks_chunk_hash_idx` ON `version_chunks` (`chunk_hash`);