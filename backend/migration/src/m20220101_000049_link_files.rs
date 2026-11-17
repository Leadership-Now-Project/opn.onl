use sea_orm::ConnectionTrait;
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // An uploaded file a short link serves. The row is made when an upload
        // starts (`pending`, no link yet) and joined to its link when the
        // upload is completed (`ready`). `deleted_at` is when the stored
        // object was removed from the bucket, not when the link was deleted.
        //
        // Both foreign keys are SET NULL: a link or an account that is purged
        // must leave the row behind, because the row is the only record of an
        // object that may still be in the bucket. The daily file sweep removes
        // the object of a `ready` row that has no link.
        manager
            .create_table(
                Table::create()
                    .table(LinkFiles::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(LinkFiles::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(LinkFiles::LinkId).integer().null())
                    .col(ColumnDef::new(LinkFiles::UserId).integer().null())
                    .col(ColumnDef::new(LinkFiles::StorageKey).text().not_null())
                    .col(ColumnDef::new(LinkFiles::Filename).text().not_null())
                    .col(ColumnDef::new(LinkFiles::ContentType).text().not_null())
                    .col(
                        ColumnDef::new(LinkFiles::SizeBytes)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(LinkFiles::Status)
                            .text()
                            .not_null()
                            .default(Expr::value("pending")),
                    )
                    .col(
                        ColumnDef::new(LinkFiles::CreatedAt)
                            .timestamp()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(ColumnDef::new(LinkFiles::DeletedAt).timestamp().null())
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_link_files_link")
                            .from(LinkFiles::Table, LinkFiles::LinkId)
                            .to(Links::Table, Links::Id)
                            .on_delete(ForeignKeyAction::SetNull)
                            .on_update(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_link_files_user")
                            .from(LinkFiles::Table, LinkFiles::UserId)
                            .to(Users::Table, Users::Id)
                            .on_delete(ForeignKeyAction::SetNull)
                            .on_update(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        let db = manager.get_connection();
        db.execute_unprepared(
            "ALTER TABLE link_files ADD CONSTRAINT link_files_status_check \
             CHECK (status IN ('pending', 'ready'))",
        )
        .await?;
        db.execute_unprepared(
            "ALTER TABLE link_files ADD CONSTRAINT link_files_size_check CHECK (size_bytes >= 0)",
        )
        .await?;
        // One object, one link. NULLs do not collide, so pending uploads and
        // rows whose link was purged are not limited by this.
        db.execute_unprepared(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_link_files_link_id ON link_files (link_id)",
        )
        .await?;
        db.execute_unprepared(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_link_files_storage_key \
             ON link_files (storage_key)",
        )
        .await?;
        // Storage use is read by uploader. Postgres does not index foreign
        // keys on its own.
        db.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_link_files_user_id ON link_files (user_id)",
        )
        .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(LinkFiles::Table).to_owned())
            .await?;
        Ok(())
    }
}

#[derive(Iden)]
enum LinkFiles {
    Table,
    Id,
    LinkId,
    UserId,
    StorageKey,
    Filename,
    ContentType,
    SizeBytes,
    Status,
    CreatedAt,
    DeletedAt,
}

#[derive(Iden)]
enum Links {
    Table,
    Id,
}

#[derive(Iden)]
enum Users {
    Table,
    Id,
}
