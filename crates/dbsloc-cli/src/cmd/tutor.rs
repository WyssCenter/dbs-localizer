// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};

use clap::{Args, Subcommand};
use dbsloc_pipeline::bids::resolve_subject_in;
use dbsloc_pipeline::paths;
use dbsloc_pipeline::tutor::{
    body_from_value, check_user_id, configured_user_id, dashboard_url, default_post_url,
    example_payload, generate_user_id, merge_payloads_in, payload_from_recon_json, post_payload,
    py_str_list, read_post_body, save_user_id, saved_user_id_path, truncate_chars, TutorError,
    UserIdSource, PROBE_HINTS,
};

use super::{bids_fail, output_dir_of};
use crate::fail::{err_line, out_json, out_line, CmdResult, Fail};

#[derive(Subcommand)]
pub enum TutorCmd {
    /// upload the markers of one sub-*_desc-reconstruction.json file and open the Lead-Tutor feedback page
    #[command(name = "submit-recon")]
    Recon(SubmitReconArgs),
    /// merge every sub-*/reconstruction/*_desc-reconstruction.json file, upload it and open the Lead-Tutor feedback page
    #[command(name = "submit-all-recon")]
    AllRecon(SubmitAllArgs),
    /// upload a ready Lead-Tutor marker JSON file
    #[command(name = "submit-file")]
    File(SubmitFileArgs),
    /// show, set or forget the Lead-Tutor user id that uploads are filed under
    #[command(name = "user-id")]
    UserId(UserIdArgs),
}

#[derive(Args)]
pub struct UserIdArgs {
    /// save this user id (letters, digits, '.', '_' and '-') for later uploads
    #[arg(long, value_name = "ID", conflicts_with = "forget")]
    pub set: Option<String>,
    /// delete the saved user id
    #[arg(long)]
    pub forget: bool,
}

#[derive(Args)]
pub struct SubmitReconArgs {
    /// path of a reconstruction JSON file (format version 2); when set, dataset_root and subj_id are not used
    #[arg(long, value_name = "PATH")]
    pub recon_json: Option<PathBuf>,
    /// BIDS root (with subj_id if not using --recon-json)
    pub dataset_root: Option<PathBuf>,
    /// subject id, with or without sub- prefix
    pub subj_id: Option<String>,
    /// find the subject's images by scanning the anat/ folders only, ignoring the rawimages.json index files
    #[arg(long)]
    pub no_index: bool,
    /// output folder of the pipeline that holds sub-<id>/ with the locks and every derived file (default: <dataset_root>/derivatives/leaddbs); a relative path is taken from the current directory. Environment variable: DBSLOC_OUTPUT_DIR
    #[arg(long, value_name = "DIR")]
    pub output_dir: Option<PathBuf>,
    /// after successful POST, do not open the feedback URL (default: open)
    #[arg(long)]
    pub no_open_browser: bool,
    /// User-ID header: letters, digits, '.', '_' and '-' (default: LEAD_TUTOR_USER_ID, else the id saved by an earlier upload or by `dbsloc tutor user-id --set`; without one, the upload asks for an id or creates a random one and saves it)
    #[arg(long)]
    pub user_id: Option<String>,
    /// upload address (default: LEAD_TUTOR_POST_URL, else http://tutor.lead-dbs.org:8000/my-endpoint)
    #[arg(long)]
    pub post_url: Option<String>,
    /// validate and print POST plan; no network
    #[arg(long)]
    pub dry_run: bool,
    /// validate only; no dry-run blurb, no network
    #[arg(long)]
    pub validate_only: bool,
}

#[derive(Args)]
pub struct SubmitAllArgs {
    /// BIDS root (uses <output folder>/sub-*/reconstruction/, by default derivatives/leaddbs/sub-*/reconstruction/)
    pub dataset_root: PathBuf,
    /// output folder of the pipeline that holds sub-<id>/ with the locks and every derived file (default: <dataset_root>/derivatives/leaddbs); a relative path is taken from the current directory. Environment variable: DBSLOC_OUTPUT_DIR
    #[arg(long, value_name = "DIR")]
    pub output_dir: Option<PathBuf>,
    /// fail unless every Lead-Tutor pt_* (ten subjects) has a recon file
    #[arg(long)]
    pub require_all: bool,
    /// after successful POST, do not open the feedback URL (default: open)
    #[arg(long)]
    pub no_open_browser: bool,
    /// User-ID header: letters, digits, '.', '_' and '-' (default: LEAD_TUTOR_USER_ID, else the id saved by an earlier upload or by `dbsloc tutor user-id --set`; without one, the upload asks for an id or creates a random one and saves it)
    #[arg(long)]
    pub user_id: Option<String>,
    /// upload address (default: LEAD_TUTOR_POST_URL, else http://tutor.lead-dbs.org:8000/my-endpoint)
    #[arg(long)]
    pub post_url: Option<String>,
    /// validate and print plan; no network
    #[arg(long)]
    pub dry_run: bool,
    /// validate only; no network
    #[arg(long)]
    pub validate_only: bool,
}

#[derive(Args)]
pub struct SubmitFileArgs {
    /// Path to JSON payload (from recon export). Omit with --print-example.
    pub json_path: Option<PathBuf>,
    /// User-ID header: letters, digits, '.', '_' and '-' (default: LEAD_TUTOR_USER_ID, else the id saved by an earlier upload or by `dbsloc tutor user-id --set`; without one, the upload asks for an id or creates a random one and saves it)
    #[arg(long)]
    pub user_id: Option<String>,
    /// upload address (default: LEAD_TUTOR_POST_URL, else http://tutor.lead-dbs.org:8000/my-endpoint)
    #[arg(long)]
    pub post_url: Option<String>,
    /// Print a minimal JSON example and exit 0
    #[arg(long)]
    pub print_example: bool,
    /// Open dashboard URL after successful POST (default: just print it)
    #[arg(long)]
    pub open_browser: bool,
    /// Load and validate JSON, print what would be sent, no network
    #[arg(long)]
    pub dry_run: bool,
    /// Load and run Lead-Tutor schema check only; no network, no dry-run blurb
    #[arg(long)]
    pub validate_only: bool,
}

pub fn run(cmd: TutorCmd) -> CmdResult {
    match cmd {
        TutorCmd::Recon(a) => submit_recon(&a),
        TutorCmd::AllRecon(a) => submit_all(&a),
        TutorCmd::File(a) => submit_file(&a),
        TutorCmd::UserId(a) => user_id_command(&a),
    }
}

fn error_fail(message: impl std::fmt::Display) -> Fail {
    Fail::new(1, format!("error: {message}"))
}

fn tutor_fail(e: &TutorError) -> Fail {
    error_fail(e)
}

fn yes_no(v: bool) -> &'static str {
    if v {
        "yes"
    } else {
        "no"
    }
}

const MAX_PROMPT_ATTEMPTS: usize = 3;
const NOT_SET_PLAN: &str = "(not set; the upload asks for one or creates a random one)";

fn usage_fail(message: impl std::fmt::Display) -> Fail {
    Fail::new(2, format!("error: {message}"))
}

fn configured_of(flag: Option<&str>) -> Result<Option<String>, Fail> {
    configured_user_id(flag)
        .map(|c| c.map(|(id, _)| id))
        .map_err(usage_fail)
}

fn prompt_user_id<R: BufRead, W: Write>(input: &mut R, output: &mut W) -> Result<String, String> {
    let _ = writeln!(
        output,
        "No Lead-Tutor user id is set. Lead-Tutor files your uploads under this id; use the \
         same id every time to compare runs."
    );
    for _ in 0..MAX_PROMPT_ATTEMPTS {
        let _ = write!(
            output,
            "Enter a user id (letters, digits, '.', '_' and '-'), or press Enter to create a \
             random one: "
        );
        let _ = output.flush();
        let mut line = String::new();
        let read = input.read_line(&mut line).map_err(|e| e.to_string())?;
        if read == 0 || line.trim().is_empty() {
            return Ok(generate_user_id());
        }
        match check_user_id(&line) {
            Ok(id) => return Ok(id),
            Err(e) => {
                let _ = writeln!(output, "{e}");
            }
        }
    }
    Err(format!(
        "no valid Lead-Tutor user id entered after {MAX_PROMPT_ATTEMPTS} attempts"
    ))
}

fn acquire_user_id() -> Result<String, Fail> {
    let id = if std::io::stdin().is_terminal() && std::io::stderr().is_terminal() {
        prompt_user_id(&mut std::io::stdin().lock(), &mut std::io::stderr()).map_err(usage_fail)?
    } else {
        generate_user_id()
    };
    match saved_user_id_path() {
        Some(path) => match save_user_id(&path, &id) {
            Ok(()) => err_line(&format!(
                "dbsloc: note: Lead-Tutor user id {id} saved in {}; later uploads use it \
                 (change it with dbsloc tutor user-id --set <id>)",
                path.display()
            )),
            Err(e) => err_line(&format!(
                "dbsloc: warning: {e}; Lead-Tutor user id {id} is used for this upload only"
            )),
        },
        None => err_line(&format!(
            "dbsloc: warning: no user data folder found to save the Lead-Tutor user id; \
             {id} is used for this upload only"
        )),
    }
    Ok(id)
}

fn user_id_command(a: &UserIdArgs) -> CmdResult {
    if let Some(raw) = &a.set {
        let id = check_user_id(raw).map_err(usage_fail)?;
        let path = saved_user_id_path().ok_or_else(|| {
            usage_fail("no user data folder found to save the Lead-Tutor user id")
        })?;
        save_user_id(&path, &id).map_err(usage_fail)?;
        out_line(&format!(
            "Lead-Tutor user id {id} saved in {}",
            path.display()
        ));
        return Ok(0);
    }
    if a.forget {
        let path = saved_user_id_path()
            .ok_or_else(|| usage_fail("no user data folder found for the Lead-Tutor user id"))?;
        match std::fs::remove_file(&path) {
            Ok(()) => out_line(&format!(
                "Deleted the saved Lead-Tutor user id {}",
                path.display()
            )),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => out_line(&format!(
                "No saved Lead-Tutor user id to forget ({} does not exist)",
                path.display()
            )),
            Err(e) => return Err(usage_fail(format!("cannot delete {}: {e}", path.display()))),
        }
        return Ok(0);
    }
    match configured_user_id(None).map_err(usage_fail)? {
        Some((id, UserIdSource::Env)) => out_line(&format!("{id} (from LEAD_TUTOR_USER_ID)")),
        Some((id, UserIdSource::Saved(path))) => {
            out_line(&format!("{id} (saved in {})", path.display()));
        }
        Some((id, UserIdSource::Flag)) => out_line(&id),
        None => out_line("not set; the next upload asks for one or creates a random one"),
    }
    Ok(0)
}

fn post_url_of(flag: Option<&String>) -> String {
    flag.cloned().unwrap_or_else(default_post_url)
}

fn open_in_browser(url: &str) {
    let program = if cfg!(target_os = "windows") {
        "explorer"
    } else if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let _ = std::process::Command::new(program).arg(url).spawn();
}

fn post_and_report(body: &[u8], user_id: &str, post_url: &str, open_browser: bool) -> CmdResult {
    let reply = match post_payload(post_url, user_id, body) {
        Ok(r) => r,
        Err(e) => {
            err_line(&format!("Network error: {e}"));
            err_line(PROBE_HINTS);
            return Ok(1);
        }
    };
    if !reply.is_success() {
        err_line(&format!("HTTP {} {}", reply.status, reply.reason));
        err_line(&truncate_chars(&reply.text, 2000));
        return Ok(1);
    }
    let dash = dashboard_url(user_id);
    if open_browser {
        open_in_browser(&dash);
    }
    let snippet = truncate_chars(&reply.text, 2000);
    out_line(&format!("HTTP {}", reply.status));
    if !snippet.trim().is_empty() {
        out_line(&format!(
            "Response (truncated): {}",
            truncate_chars(&snippet, 500)
        ));
    }
    out_line("Open in browser (Lead-Tutor feedback):");
    out_line(&dash);
    Ok(0)
}

fn submit_recon(a: &SubmitReconArgs) -> CmdResult {
    let rpath: PathBuf = match &a.recon_json {
        Some(p) => paths::absolute(p),
        None => {
            let (Some(root), Some(subj)) = (&a.dataset_root, &a.subj_id) else {
                return Err(Fail::new(
                    2,
                    "error: use --recon-json PATH, or both dataset_root and subj_id",
                ));
            };
            let out = output_dir_of(a.output_dir.as_deref());
            let sub = resolve_subject_in(&paths::absolute(root), out.as_deref(), subj, !a.no_index)
                .map_err(|e| bids_fail(&e))?;
            sub.reconstruction_json()
        }
    };
    let payload = payload_from_recon_json(&rpath).map_err(|e| tutor_fail(&e))?;
    let body = body_from_value(&payload).map_err(error_fail)?;
    if a.validate_only {
        out_line(&format!("Lead-Tutor payload OK, bytes: {}", body.len()));
        return Ok(0);
    }
    let configured = configured_of(a.user_id.as_deref())?;
    let post_url = post_url_of(a.post_url.as_ref());
    if a.dry_run {
        out_line(&format!("dry-run: would POST {} bytes", body.len()));
        out_line(&format!("recon path: {}", rpath.display()));
        out_line(&format!(
            "User-ID: {}",
            configured.as_deref().unwrap_or(NOT_SET_PLAN)
        ));
        out_line(&format!("POST URL: {post_url}"));
        out_line(&format!("open browser: {}", yes_no(!a.no_open_browser)));
        return Ok(0);
    }
    let user_id = match configured {
        Some(id) => id,
        None => acquire_user_id()?,
    };
    post_and_report(&body, &user_id, &post_url, !a.no_open_browser)
}

fn submit_all(a: &SubmitAllArgs) -> CmdResult {
    let out = output_dir_of(a.output_dir.as_deref());
    let merged = merge_payloads_in(&a.dataset_root, out.as_deref(), a.require_all)
        .map_err(|e| tutor_fail(&e))?;
    for note in &merged.skipped {
        err_line(&format!("dbsloc: warning: {note}"));
    }
    let payload = merged.payload;
    let patients: Vec<String> = payload
        .as_object()
        .map(|o| o.keys().cloned().collect())
        .unwrap_or_default();
    if patients.is_empty() {
        return Err(error_fail(
            "no reconstruction JSON found under derivatives/leaddbs or the chosen output folder",
        ));
    }
    let body = body_from_value(&payload).map_err(error_fail)?;
    if a.validate_only {
        out_line(&format!(
            "Lead-Tutor merged payload OK, patients: {} bytes: {}",
            patients.len(),
            body.len()
        ));
        return Ok(0);
    }
    let configured = configured_of(a.user_id.as_deref())?;
    let post_url = post_url_of(a.post_url.as_ref());
    if a.dry_run {
        out_line(&format!(
            "dry-run: would POST {} bytes, patients {}",
            body.len(),
            py_str_list(&patients)
        ));
        out_line(&format!(
            "User-ID: {}",
            configured.as_deref().unwrap_or(NOT_SET_PLAN)
        ));
        out_line(&format!("POST URL: {post_url}"));
        out_line(&format!("open browser: {}", yes_no(!a.no_open_browser)));
        return Ok(0);
    }
    let user_id = match configured {
        Some(id) => id,
        None => acquire_user_id()?,
    };
    post_and_report(&body, &user_id, &post_url, !a.no_open_browser)
}

fn submit_file(a: &SubmitFileArgs) -> CmdResult {
    if a.print_example {
        out_json(&example_payload());
        return Ok(0);
    }
    let Some(json_path) = &a.json_path else {
        return Err(Fail::new(
            2,
            "dbsloc tutor submit-file: error: json_path required (or use --print-example)",
        ));
    };
    let path: &Path = &paths::absolute(json_path);
    if !path.is_file() {
        return Err(Fail::new(
            1,
            format!("error: not a file: {}", path.display()),
        ));
    }
    let body = read_post_body(path).map_err(|e| Fail::new(1, e))?;
    if a.validate_only {
        out_line(&format!(
            "Lead-Tutor JSON OK, payload bytes: {}",
            body.len()
        ));
        return Ok(0);
    }
    let configured = configured_of(a.user_id.as_deref())?;
    let post_url = post_url_of(a.post_url.as_ref());
    if a.dry_run {
        out_line(&format!("dry-run: JSON OK, bytes: {}", body.len()));
        out_line(&format!("Would POST to: {post_url}"));
        out_line(&format!(
            "User-ID: {}",
            configured.as_deref().unwrap_or(NOT_SET_PLAN)
        ));
        return Ok(0);
    }
    let user_id = match configured {
        Some(id) => id,
        None => acquire_user_id()?,
    };
    post_and_report(&body, &user_id, &post_url, a.open_browser)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn ask(input: &str) -> (Result<String, String>, String) {
        let mut out = Vec::new();
        let got = prompt_user_id(&mut input.as_bytes(), &mut out);
        (got, String::from_utf8(out).unwrap())
    }

    fn is_generated(id: &str) -> bool {
        id.strip_prefix("dbsloc-")
            .is_some_and(|h| h.len() == 12 && h.chars().all(|c| c.is_ascii_hexdigit()))
    }

    #[test]
    fn a_typed_valid_id_is_accepted() {
        let (got, shown) = ask("  run-7 \n");
        assert_eq!(got.unwrap(), "run-7");
        assert!(shown.contains("No Lead-Tutor user id is set"), "{shown}");
        assert!(
            shown.contains("press Enter to create a random one"),
            "{shown}"
        );
    }

    #[test]
    fn an_empty_line_or_end_of_input_creates_a_random_id() {
        assert!(is_generated(&ask("\n").0.unwrap()));
        assert!(is_generated(&ask("   \n").0.unwrap()));
        assert!(is_generated(&ask("").0.unwrap()));
    }

    #[test]
    fn an_invalid_id_asks_again() {
        let (got, shown) = ask("a b\nrun-7\n");
        assert_eq!(got.unwrap(), "run-7");
        assert!(shown.contains("invalid User-ID"), "{shown}");
    }

    #[test]
    fn three_invalid_ids_fail() {
        let (got, shown) = ask("a b\nx&y\n/\nrun-7\n");
        assert!(got.unwrap_err().contains("after 3 attempts"));
        assert_eq!(shown.matches("invalid User-ID").count(), 3, "{shown}");
    }
}
