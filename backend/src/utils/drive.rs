//! Google Drive links: a short link made for a file picked in Google's file
//! picker. The picking, and every call to Drive, happens in the browser; this
//! server never sees a Google token. It hands the dashboard the picker's
//! public settings and remembers which Drive file a link was made for.

use sea_orm::{ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter};
use serde::Deserialize;
use utoipa::ToSchema;

use crate::entity::{link_drive_files, links};
use crate::utils::env::env_nonempty;

/// What the dashboard needs to open Google's file picker. Both values are
/// public by design: the key is a browser key restricted to the dashboard's
/// address, and the app id is the Cloud project's number.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DrivePicker {
    pub api_key: String,
    pub app_id: String,
}

impl DrivePicker {
    /// `GOOGLE_PICKER_API_KEY` and `GOOGLE_PICKER_APP_ID`, on an instance where
    /// Google sign-in is configured. Read once at startup.
    pub fn from_env() -> Option<Self> {
        Self::from_parts(
            env_nonempty("GOOGLE_PICKER_API_KEY"),
            env_nonempty("GOOGLE_PICKER_APP_ID"),
            crate::handlers::google_auth::configured_client_id().is_some(),
        )
    }

    /// The picker needs both settings and the Google sign-in the Drive token
    /// is asked for with. Anything less is "not set up".
    pub fn from_parts(
        api_key: Option<String>,
        app_id: Option<String>,
        google_sign_in: bool,
    ) -> Option<Self> {
        if !google_sign_in {
            return None;
        }
        Some(Self {
            api_key: api_key?.trim().to_string(),
            app_id: app_id?.trim().to_string(),
        })
    }
}

/// The Drive file a new link is for, as `POST /links` takes it.
#[derive(Deserialize, ToSchema, Clone, Debug)]
pub struct DriveFileRef {
    /// Drive's id of the file.
    pub file_id: String,
    /// The file's name in Drive.
    pub name: String,
    pub mime_type: String,
}

// The dashboard applies these same limits before it asks Google to change a
// file's sharing (`DRIVE_LINK_LIMITS`, `driveLinkProblem`, `cleanDriveName`
// and `isDriveLinkUrl` in `frontend/src/utils/googleDrive.ts`): a link refused
// here after that change would leave the file public with no link. Change
// both sides together.
const MAX_FILE_ID_LEN: usize = 200;
const MAX_NAME_CHARS: usize = 500;
const MAX_MIME_TYPE_LEN: usize = 255;

impl DriveFileRef {
    /// The reference with its text trimmed, or why it is refused.
    pub fn checked(&self) -> Result<Self, &'static str> {
        let file_id = self.file_id.trim();
        if file_id.is_empty()
            || file_id.len() > MAX_FILE_ID_LEN
            || !file_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        {
            return Err("drive_file.file_id is not a Google Drive file id");
        }
        let name = self.name.trim();
        if name.is_empty() || name.chars().count() > MAX_NAME_CHARS {
            return Err("drive_file.name must be 1 to 500 characters");
        }
        if name.chars().any(char::is_control) {
            return Err("drive_file.name contains control characters");
        }
        let mime_type = self.mime_type.trim();
        if mime_type.len() > MAX_MIME_TYPE_LEN {
            return Err("drive_file.mime_type is too long");
        }
        if mime_type.chars().any(char::is_control) {
            return Err("drive_file.mime_type contains control characters");
        }
        Ok(Self {
            file_id: file_id.to_string(),
            name: name.to_string(),
            mime_type: mime_type.to_string(),
        })
    }
}

/// Whether `url` is an address Google Drive shows a file at: `https` on
/// `docs.google.com` or `drive.google.com`, on the default port, written out
/// in full. `https:drive.google.com/…` parses to the same host but is not an
/// address Drive hands out, and a browser would not open Drive with it.
///
/// Twin: `isDriveLinkUrl` in `frontend/src/utils/googleDrive.ts`.
pub fn is_drive_url(url: &str) -> bool {
    if !crate::utils::url_policy::is_written_out_http_url(url) {
        return false;
    }
    let Ok(parsed) = url::Url::parse(url) else {
        return false;
    };
    parsed.scheme() == "https"
        && parsed.port().is_none()
        && matches!(
            parsed.host_str(),
            Some("docs.google.com" | "drive.google.com")
        )
}

/// Forget which Drive files these links were made for: the links are being
/// deleted. Only a link that points into Drive can have a row, so deleting any
/// other link runs no query here.
pub async fn forget_for_links<'a, C: ConnectionTrait>(
    db: &C,
    links: impl IntoIterator<Item = &'a links::Model>,
) -> Result<(), DbErr> {
    let ids: Vec<i32> = links
        .into_iter()
        .filter(|link| is_drive_url(&link.original_url))
        .map(|link| link.id)
        .collect();
    if ids.is_empty() {
        return Ok(());
    }
    link_drive_files::Entity::delete_many()
        .filter(link_drive_files::Column::LinkId.is_in(ids))
        .exec(db)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn some(value: &str) -> Option<String> {
        Some(value.to_string())
    }

    #[test]
    fn the_picker_needs_both_settings_and_google_sign_in() {
        assert_eq!(
            DrivePicker::from_parts(some("key"), some("123"), true),
            Some(DrivePicker {
                api_key: "key".into(),
                app_id: "123".into()
            })
        );
        assert_eq!(DrivePicker::from_parts(None, some("123"), true), None);
        assert_eq!(DrivePicker::from_parts(some("key"), None, true), None);
        assert_eq!(
            DrivePicker::from_parts(some("key"), some("123"), false),
            None
        );
    }

    #[test]
    fn only_https_drive_hosts_are_drive_urls() {
        assert!(is_drive_url("https://docs.google.com/document/d/abc/edit"));
        assert!(is_drive_url("https://drive.google.com/file/d/abc/view"));
        assert!(!is_drive_url("http://drive.google.com/file/d/abc/view"));
        assert!(!is_drive_url(
            "https://drive.google.com.evil.test/file/d/abc"
        ));
        assert!(!is_drive_url(
            "https://evil.test/?u=https://drive.google.com/"
        ));
        assert!(!is_drive_url(
            "https://user@evil.test:443@drive.google.com.evil.test/"
        ));
        assert!(!is_drive_url("https://drive.google.com:8443/file/d/abc"));
        assert!(!is_drive_url("https://sites.google.com/view/abc"));
        assert!(!is_drive_url("opn-file:12"));
    }

    /// The parser reads each of these as `https://drive.google.com/…`; none is
    /// an address Drive hands out, and a browser would not open Drive with
    /// the first two.
    #[test]
    fn a_drive_address_that_is_not_written_out_is_not_a_drive_url() {
        for url in [
            "https:drive.google.com/file/d/abc/view",
            "https:/drive.google.com/file/d/abc/view",
            "https:///drive.google.com/file/d/abc/view",
            "https:\\\\drive.google.com\\file\\d\\abc\\view",
            "https://drive.google.com\\file/d/abc/view",
            " https://drive.google.com/file/d/abc/view",
            "https://drive.google.com/file/d/abc/view ",
        ] {
            assert!(
                url::Url::parse(url)
                    .is_ok_and(|parsed| parsed.host_str() == Some("drive.google.com")),
                "{url:?} is one the parser accepts"
            );
            assert!(!is_drive_url(url), "{url:?}");
        }
        assert!(is_drive_url("HTTPS://drive.google.com/file/d/abc/view"));
    }

    fn file(file_id: &str, name: &str, mime_type: &str) -> DriveFileRef {
        DriveFileRef {
            file_id: file_id.into(),
            name: name.into(),
            mime_type: mime_type.into(),
        }
    }

    #[test]
    fn a_file_reference_is_checked_and_trimmed() {
        let ok = file(" 1AbC_d-9 ", " Plan.pdf ", "application/pdf")
            .checked()
            .unwrap();
        assert_eq!(ok.file_id, "1AbC_d-9");
        assert_eq!(ok.name, "Plan.pdf");

        assert!(file("", "a", "x/y").checked().is_err());
        assert!(file("abc/def", "a", "x/y").checked().is_err());
        assert!(file("abc def", "a", "x/y").checked().is_err());
        assert!(file(&"a".repeat(201), "a", "x/y").checked().is_err());
        assert!(file("abc", "", "x/y").checked().is_err());
        assert!(file("abc", &"n".repeat(501), "x/y").checked().is_err());
        assert!(file("abc", "a\nb", "x/y").checked().is_err());
        assert!(file("abc", "a", &"m".repeat(256)).checked().is_err());
        assert!(file("abc", "a", &"m".repeat(255)).checked().is_ok());
        // A control character inside the type (one at the end is trimmed).
        assert_eq!(
            file("abc", "a", "text/\u{7}plain").checked().unwrap_err(),
            "drive_file.mime_type contains control characters"
        );
        assert!(file("abc", "a", "text/pl\nain").checked().is_err());
        // Drive does not always name a type.
        assert!(file("abc", "a", "").checked().is_ok());
    }
}
