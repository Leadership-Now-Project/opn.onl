export interface LinkData {
    id: number;
    code: string;
    original_url: string;
    short_url: string;
    api_url?: string;
    title: string | null;
    click_count: number;
    created_at: string;
    expires_at: string | null;
    starts_at?: string | null;
    max_clicks?: number | null;
    has_password: boolean;
    notes: string | null;
    is_active: boolean;
    is_pinned: boolean;
    burn_after_reading?: boolean;
    burned_at?: string | null;
    safe_link_interstitial?: boolean;
    bio_visible?: boolean;
    folder_id?: number | null;
    org_id?: number | null;
    tags: { id: number; name: string; color?: string | null }[];
    /** Who created the link. Only in an organization's list (GET /links?org_id=). */
    created_by?: { name: string | null; email: string } | null;
    /** False when the API would refuse this caller's delete. Only in an organization's list. */
    can_delete?: boolean;
    /** The uploaded file a file link serves. `original_url` is then empty. */
    file?: LinkFile | null;
    /** The Google Drive file the link was made for. `original_url` is its address in Drive. */
    drive?: LinkDriveFile | null;
    /** Only on a media link: it opens a page with the video's player instead of redirecting. */
    media?: LinkMedia | null;
}

export interface LinkMedia {
    /** `youtube`, `vimeo` or `drive`. */
    provider: string;
    /** The button under the player, when the link has one. */
    cta_label?: string | null;
    cta_url?: string | null;
    /** Only in the answer to creating the link: `not_checked` when the video's host could not be asked. */
    embed_check?: string | null;
}

export interface LinkDriveFile {
    name: string;
    mime_type: string;
}

export interface LinkFile {
    filename: string;
    size_bytes: number;
    content_type: string;
    /** Secure View is on: the link opens the viewer instead of handing the file over. */
    secure_view?: boolean;
    /** Whether Secure View can be switched on for this file (PDF and the four image types). */
    secure_view_eligible?: boolean;
}

export interface RoutingRule {
    match_device?: string | null;
    match_os?: string | null;
    match_country?: string | null;
    match_lang?: string | null;
    destination_url: string;
    weight?: number;
    priority?: number;
}

export interface LinkUpdatePayload {
    original_url?: string;
    title?: string;
    password?: string;
    expires_at?: string;
    remove_password?: boolean;
    remove_expiration?: boolean;
    notes?: string;
    starts_at?: string;
    max_clicks?: number;
    remove_starts_at?: boolean;
    remove_max_clicks?: boolean;
    burn_after_reading?: boolean;
    safe_link_interstitial?: boolean;
    bio_visible?: boolean;
    folder_id?: number;
    remove_folder?: boolean;
    /** The button under a media link's player. Both empty removes it. */
    media_cta?: { label: string; url: string };
}

export interface AppSettings {
    custom_aliases_enabled: boolean;
    min_alias_length: number;
    max_alias_length: number;
    qr_branding_enabled: boolean;
    burn_after_reading_enabled: boolean;
    safe_link_interstitial_enabled: boolean;
    conditional_routing_enabled: boolean;
    link_in_bio_enabled: boolean;
    /** The shared organization every staff account joins, when the instance has one. */
    default_org?: { id: number; name: string } | null;
    /** File sharing. Absent on a server with no storage for uploaded files. */
    files?: { enabled: boolean; max_bytes: number } | null;
    /** Google sign-in's client id. Absent on a server that does not sign people in with Google. */
    google_client_id?: string | null;
    /** Google's file picker. Absent on a server where it is not set up. */
    drive?: { api_key: string; app_id: string } | null;
}

export type LinkSortBy = 'date' | 'clicks';
export type LinkSortOrder = 'asc' | 'desc';

export interface SparklineSeries {
    data: number[];
    labels: string[];
}
