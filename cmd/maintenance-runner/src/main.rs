use serde::{Deserialize, Serialize};
use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::{SystemTime, UNIX_EPOCH};

const DEFAULT_PLAN_PATH: &str = "/run/kaniop-maintenance/plan.json";
const DEFAULT_DATA_PATH: &str = "/data";
const MARKER_DIR: &str = ".kaniop-maintenance";
const PLAN_VERSION: u32 = 1;
const EXIT_FAILED_MARKER: u8 = 42;
const EXIT_INTERRUPTED_MARKER: u8 = 43;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum Operation {
    Reindex,
    Verify,
    Vacuum,
}

impl Operation {
    fn as_arg(self) -> &'static str {
        match self {
            Self::Reindex => "reindex",
            Self::Verify => "verify",
            Self::Vacuum => "vacuum",
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MaintenancePlan {
    version: u32,
    active: bool,
    operation_id: String,
    pod_name: String,
    operation: Operation,
    #[serde(default)]
    retry_interrupted: bool,
    #[serde(default)]
    config_path: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum MarkerState {
    Running,
    Completed,
    Failed,
    Interrupted,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Marker {
    operation_id: String,
    pod_name: String,
    operation: Operation,
    state: MarkerState,
    #[serde(skip_serializing_if = "Option::is_none")]
    exit_code: Option<i32>,
    updated_at_unix_seconds: u64,
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(error) => {
            eprintln!("kaniop maintenance runner: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<ExitCode, String> {
    let mut args = env::args().skip(1);
    match args.next().as_deref() {
        Some("install") => {
            let destination = args
                .next()
                .ok_or_else(|| "install requires a destination path".to_string())?;
            if args.next().is_some() {
                return Err("install accepts exactly one destination path".to_string());
            }
            install(Path::new(&destination))?;
            Ok(ExitCode::SUCCESS)
        }
        Some("execute") => {
            if args.next().is_some() {
                return Err("execute does not accept positional arguments".to_string());
            }
            execute()
        }
        Some(command) => Err(format!("unknown command '{command}'")),
        None => Err("expected 'install' or 'execute'".to_string()),
    }
}

fn install(destination: &Path) -> Result<(), String> {
    let source = env::current_exe().map_err(|e| format!("resolve current executable: {e}"))?;
    let parent = destination
        .parent()
        .ok_or_else(|| format!("destination {} has no parent", destination.display()))?;
    fs::create_dir_all(parent)
        .map_err(|e| format!("create destination directory {}: {e}", parent.display()))?;
    fs::copy(&source, destination).map_err(|e| {
        format!(
            "copy maintenance runner from {} to {}: {e}",
            source.display(),
            destination.display()
        )
    })?;
    let mut permissions = fs::metadata(destination)
        .map_err(|e| format!("stat {}: {e}", destination.display()))?
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(destination, permissions)
        .map_err(|e| format!("chmod {}: {e}", destination.display()))?;
    Ok(())
}

fn execute() -> Result<ExitCode, String> {
    let plan_path =
        env::var("KANIOP_MAINTENANCE_PLAN").unwrap_or_else(|_| DEFAULT_PLAN_PATH.to_string());
    let plan_path = Path::new(&plan_path);
    if !plan_path.exists() {
        return Ok(ExitCode::SUCCESS);
    }

    let plan: MaintenancePlan = serde_json::from_slice(
        &fs::read(plan_path).map_err(|e| format!("read {}: {e}", plan_path.display()))?,
    )
    .map_err(|e| format!("parse {}: {e}", plan_path.display()))?;

    if plan.version != PLAN_VERSION {
        return Err(format!(
            "unsupported maintenance plan version {}, expected {PLAN_VERSION}",
            plan.version
        ));
    }
    if !plan.active {
        return Ok(ExitCode::SUCCESS);
    }
    validate_operation_id(&plan.operation_id)?;

    let pod_name = env::var("POD_NAME").map_err(|_| "POD_NAME is not set".to_string())?;
    if pod_name != plan.pod_name {
        return Ok(ExitCode::SUCCESS);
    }

    let data_path =
        env::var("KANIOP_MAINTENANCE_DATA_PATH").unwrap_or_else(|_| DEFAULT_DATA_PATH.to_string());
    let marker_dir = Path::new(&data_path).join(MARKER_DIR);
    fs::create_dir_all(&marker_dir)
        .map_err(|e| format!("create marker directory {}: {e}", marker_dir.display()))?;
    let marker_path = marker_dir.join(format!("{}.json", plan.operation_id));

    if let Some(marker) = read_marker(&marker_path)? {
        validate_marker(&plan, &pod_name, &marker)?;
        match marker.state {
            MarkerState::Completed => {
                eprintln!(
                    "maintenance operation {} already completed on {}",
                    plan.operation_id, pod_name
                );
                return Ok(ExitCode::SUCCESS);
            }
            MarkerState::Failed => {
                eprintln!(
                    "maintenance operation {} previously failed on {}; refusing to execute it again",
                    plan.operation_id, pod_name
                );
                return Ok(ExitCode::from(EXIT_FAILED_MARKER));
            }
            MarkerState::Interrupted if !plan.retry_interrupted => {
                eprintln!(
                    "maintenance operation {} was interrupted on {}; automatic retry is disabled",
                    plan.operation_id, pod_name
                );
                return Ok(ExitCode::from(EXIT_INTERRUPTED_MARKER));
            }
            MarkerState::Running if !plan.retry_interrupted => {
                write_marker(
                    &marker_path,
                    marker_for(&plan, &pod_name, MarkerState::Interrupted, None),
                )?;
                eprintln!(
                    "maintenance operation {} appears to have been interrupted on {}; automatic retry is disabled",
                    plan.operation_id, pod_name
                );
                return Ok(ExitCode::from(EXIT_INTERRUPTED_MARKER));
            }
            MarkerState::Running | MarkerState::Interrupted => {
                eprintln!(
                    "retrying interrupted maintenance operation {} on {}",
                    plan.operation_id, pod_name
                );
            }
        }
    }

    write_marker(
        &marker_path,
        marker_for(&plan, &pod_name, MarkerState::Running, None),
    )?;

    let mut command = Command::new("kanidmd");
    command.args(["database", plan.operation.as_arg()]);
    if let Some(config_path) = plan
        .config_path
        .as_deref()
        .filter(|p| Path::new(p).exists())
    {
        command.args(["-c", config_path]);
    }

    eprintln!(
        "starting Kanidm database {} for operation {} on {}",
        plan.operation.as_arg(),
        plan.operation_id,
        pod_name
    );
    let status = command
        .status()
        .map_err(|e| format!("start kanidmd database {}: {e}", plan.operation.as_arg()))?;

    if status.success() {
        write_marker(
            &marker_path,
            marker_for(&plan, &pod_name, MarkerState::Completed, status.code()),
        )?;
        eprintln!(
            "maintenance operation {} completed on {}",
            plan.operation_id, pod_name
        );
        Ok(ExitCode::SUCCESS)
    } else {
        write_marker(
            &marker_path,
            marker_for(&plan, &pod_name, MarkerState::Failed, status.code()),
        )?;
        Err(format!(
            "kanidmd database {} failed for operation {} on {} with status {}",
            plan.operation.as_arg(),
            plan.operation_id,
            pod_name,
            status
        ))
    }
}

fn validate_operation_id(operation_id: &str) -> Result<(), String> {
    if operation_id.is_empty()
        || operation_id.len() > 128
        || !operation_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err("operationId contains unsupported characters".to_string());
    }
    Ok(())
}

fn validate_marker(plan: &MaintenancePlan, pod_name: &str, marker: &Marker) -> Result<(), String> {
    if marker.operation_id != plan.operation_id
        || marker.pod_name != pod_name
        || marker.operation != plan.operation
    {
        return Err(format!(
            "marker {} does not match the active maintenance plan",
            plan.operation_id
        ));
    }
    Ok(())
}

fn marker_for(
    plan: &MaintenancePlan,
    pod_name: &str,
    state: MarkerState,
    exit_code: Option<i32>,
) -> Marker {
    Marker {
        operation_id: plan.operation_id.clone(),
        pod_name: pod_name.to_string(),
        operation: plan.operation,
        state,
        exit_code,
        updated_at_unix_seconds: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
    }
}

fn read_marker(path: &Path) -> Result<Option<Marker>, String> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|e| format!("parse marker {}: {e}", path.display())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("read marker {}: {error}", path.display())),
    }
}

fn write_marker(path: &Path, marker: Marker) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("marker path {} has no parent", path.display()))?;
    let temporary = temporary_path(path);
    let bytes = serde_json::to_vec(&marker).map_err(|e| format!("serialize marker: {e}"))?;

    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|e| format!("create marker {}: {e}", temporary.display()))?;
    file.write_all(&bytes)
        .map_err(|e| format!("write marker {}: {e}", temporary.display()))?;
    file.write_all(b"\n")
        .map_err(|e| format!("finish marker {}: {e}", temporary.display()))?;
    file.sync_all()
        .map_err(|e| format!("sync marker {}: {e}", temporary.display()))?;
    drop(file);

    fs::rename(&temporary, path).map_err(|e| {
        format!(
            "atomically replace marker {} with {}: {e}",
            path.display(),
            temporary.display()
        )
    })?;
    sync_directory(parent)?;
    Ok(())
}

fn temporary_path(path: &Path) -> PathBuf {
    let mut temporary = path.as_os_str().to_os_string();
    temporary.push(format!(".tmp-{}", std::process::id()));
    PathBuf::from(temporary)
}

fn sync_directory(path: &Path) -> Result<(), String> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|e| format!("sync directory {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_plan(operation: Operation) -> MaintenancePlan {
        MaintenancePlan {
            version: PLAN_VERSION,
            active: true,
            operation_id: "1234-abcd".to_string(),
            pod_name: "example-default-0".to_string(),
            operation,
            retry_interrupted: false,
            config_path: None,
        }
    }

    #[test]
    fn operation_arguments_match_kanidmd_cli() {
        assert_eq!(Operation::Reindex.as_arg(), "reindex");
        assert_eq!(Operation::Verify.as_arg(), "verify");
        assert_eq!(Operation::Vacuum.as_arg(), "vacuum");
    }

    #[test]
    fn marker_must_match_operation_and_pod() {
        let plan = make_plan(Operation::Reindex);
        let marker = marker_for(&plan, &plan.pod_name, MarkerState::Completed, Some(0));
        assert!(validate_marker(&plan, &plan.pod_name, &marker).is_ok());

        let other_plan = make_plan(Operation::Verify);
        assert!(validate_marker(&other_plan, &other_plan.pod_name, &marker).is_err());
    }

    #[test]
    fn operation_id_is_safe_as_a_filename() {
        assert!(validate_operation_id("52a82eb4-5d48-4691-a973-42b050fa80cd").is_ok());
        assert!(validate_operation_id("../escape").is_err());
        assert!(validate_operation_id("").is_err());
    }
}
