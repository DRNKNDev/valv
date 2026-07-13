ALTER TABLE "folder_grants" ADD COLUMN "name" text;--> statement-breakpoint
ALTER TABLE "folder_grants" ADD COLUMN "created_by_user_id" text;--> statement-breakpoint
CREATE UNIQUE INDEX "folder_grants_folder_name_unique" ON "folder_grants" USING btree ("folder_id","name") WHERE "folder_grants"."device_id" IS NOT NULL;