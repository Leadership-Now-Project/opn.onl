use sea_orm::ConnectionTrait;
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // The Google Drive file a short link was made for. The link itself is
        // an ordinary URL link to the file's address in Drive; this row only
        // remembers which file it is and what it was called when picked.
        //
        // The link's foreign key is CASCADE, unlike `link_files`: nothing is
        // stored outside the database, so a purged link takes the row with it.
        manager
            .create_table(
                Table::create()
                    .table(LinkDriveFiles::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(LinkDriveFiles::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(LinkDriveFiles::LinkId).integer().not_null())
                    .col(ColumnDef::new(LinkDriveFiles::FileId).text().not_null())
                    .col(ColumnDef::new(LinkDriveFiles::Name).text().not_null())
                    .col(ColumnDef::new(LinkDriveFiles::MimeType).text().not_null())
                    .col(ColumnDef::new(LinkDriveFiles::CreatedBy).integer().null())
                    .col(
                        ColumnDef::new(LinkDriveFiles::CreatedAt)
                            .timestamp()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_link_drive_files_link")
                            .from(LinkDriveFiles::Table, LinkDriveFiles::LinkId)
                            .to(Links::Table, Links::Id)
                            .on_delete(ForeignKeyAction::Cascade)
                            .on_update(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_link_drive_files_created_by")
                            .from(LinkDriveFiles::Table, LinkDriveFiles::CreatedBy)
                            .to(Users::Table, Users::Id)
                            .on_delete(ForeignKeyAction::SetNull)
                            .on_update(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        // One link, one Drive file. Also the index the link lists look rows up by.
        manager
            .get_connection()
            .execute_unprepared(
                "CREATE UNIQUE INDEX IF NOT EXISTS idx_link_drive_files_link_id \
                 ON link_drive_files (link_id)",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(LinkDriveFiles::Table).to_owned())
            .await?;
        Ok(())
    }
}

#[derive(Iden)]
enum LinkDriveFiles {
    Table,
    Id,
    LinkId,
    FileId,
    Name,
    MimeType,
    CreatedBy,
    CreatedAt,
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
