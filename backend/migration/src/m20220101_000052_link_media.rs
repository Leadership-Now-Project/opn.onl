use sea_orm::ConnectionTrait;
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // A media link: a short link that opens a page with a video player on
        // it instead of redirecting. The link itself stays an ordinary URL
        // link to the video's own page (YouTube, Vimeo, Google Drive); this
        // row says which video it is and what the page's button says.
        //
        // Additive: a new table and nothing else. The foreign key is CASCADE,
        // as for `link_drive_files`: nothing is stored outside the database,
        // so a purged link takes the row with it.
        manager
            .get_connection()
            .execute_unprepared(
                "CREATE TABLE IF NOT EXISTS link_media ( \
                    id SERIAL PRIMARY KEY, \
                    link_id INTEGER NOT NULL \
                        CONSTRAINT fk_link_media_link REFERENCES links (id) \
                        ON DELETE CASCADE ON UPDATE CASCADE, \
                    provider TEXT NOT NULL \
                        CONSTRAINT link_media_provider_check \
                        CHECK (provider IN ('youtube', 'vimeo', 'drive')), \
                    media_id TEXT NOT NULL, \
                    cta_label TEXT NULL, \
                    cta_url TEXT NULL, \
                    created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP \
                 )",
            )
            .await?;

        // One link, one video. Also the index every lookup goes through.
        manager
            .get_connection()
            .execute_unprepared(
                "CREATE UNIQUE INDEX IF NOT EXISTS idx_link_media_link_id \
                 ON link_media (link_id)",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP TABLE IF EXISTS link_media")
            .await?;
        Ok(())
    }
}
