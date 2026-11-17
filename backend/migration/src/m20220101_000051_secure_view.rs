use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        // Secure View: the link opens a viewer that asks for an email instead
        // of handing over the file. `protected_key` is where the copy of a PDF
        // stamped "no printing, no copying" is stored, when one was made.
        // `secure_view_generation` goes up each time the switch is turned, and
        // every ticket and view token names the one it was issued under, so
        // switching off ends them for good.
        // link_files is small, and a constant default is a catalog-only change
        // on Postgres 11 and later.
        db.execute_unprepared(
            "ALTER TABLE link_files \
             ADD COLUMN IF NOT EXISTS secure_view BOOLEAN NOT NULL DEFAULT FALSE, \
             ADD COLUMN IF NOT EXISTS protected_key TEXT NULL, \
             ADD COLUMN IF NOT EXISTS secure_view_generation INTEGER NOT NULL DEFAULT 0",
        )
        .await?;
        // A view ticket opens the view once. The id of each ticket that did is
        // kept here until the ticket would have expired anyway (15 minutes);
        // the daily file sweep removes the rest. A handful of rows.
        db.execute_unprepared(
            "CREATE TABLE IF NOT EXISTS secure_view_tickets ( \
                 id TEXT PRIMARY KEY, \
                 expires_at TIMESTAMP NOT NULL \
             )",
        )
        .await?;
        // The email a visitor gave the Secure View viewer. click_events is the
        // largest table: a nullable column without a default is a catalog-only
        // change (no rewrite, no scan), as `destination` was. No index: it is
        // only ever read with the link's other clicks.
        db.execute_unprepared(
            "ALTER TABLE click_events ADD COLUMN IF NOT EXISTS viewer_email TEXT NULL",
        )
        .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared("DROP TABLE IF EXISTS secure_view_tickets")
            .await?;
        db.execute_unprepared("ALTER TABLE click_events DROP COLUMN IF EXISTS viewer_email")
            .await?;
        db.execute_unprepared(
            "ALTER TABLE link_files \
             DROP COLUMN IF EXISTS secure_view_generation, \
             DROP COLUMN IF EXISTS protected_key, \
             DROP COLUMN IF EXISTS secure_view",
        )
        .await?;
        Ok(())
    }
}
