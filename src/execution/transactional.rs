use super::Executor;
use crate::{config::CompilerSetting, utils::create_tmp_move_file};
use anyhow::Result;
use log::error;
#[cfg(feature = "git_deps")]
use move_model::metadata::LanguageVersion;
#[cfg(feature = "local_deps")]
use move_model_local::metadata::LanguageVersion;
#[cfg(feature = "git_deps")]
use move_transactional_test_runner::{vm_test_harness, vm_test_harness::TestRunConfig};
#[cfg(feature = "local_deps")]
use move_transactional_test_runner_local::{vm_test_harness, vm_test_harness::TestRunConfig};
use once_cell::sync::Lazy;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    error::Error,
    fmt::Display,
    panic,
    path::PathBuf,
    time::{Duration, Instant},
};
use tempfile::TempDir;

pub struct TransactionalRunner {
    saved_results: BTreeSet<TransactionalResult>,
}

pub struct TransactionalInput {
    pub code: String,
    pub config: CompilerSetting,
}

#[derive(Default, Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
pub struct TransactionalResult {
    pub log: String,
    pub status: ResultStatus,
    pub v1_chunks: Vec<ResultChunk>,
    pub v2_chunks: Vec<ResultChunk>,
    pub duration: Duration,
}

#[derive(Default, Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
pub enum ResultStatus {
    Success,
    Failure,
    #[default]
    Unknown,
}

#[derive(Default, Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
pub struct ResultChunk {
    pub canonical: String,
    pub kind: ResultChunkKind,
    pub lines: Vec<String>,
}

#[derive(Default, Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
pub enum ResultChunkKind {
    #[default]
    Task,
    Error,
    VMError,
    Bug,
    Panic,
    Warning,
}

impl ResultChunkKind {
    pub fn try_from_str(msg: &str) -> Option<Self> {
        return if msg.contains("warning") {
            Some(Self::Warning)
        } else if msg.contains("task") {
            Some(Self::Task)
        } else if msg.contains("VMError") {
            Some(Self::VMError)
        } else if msg.starts_with("error") {
            Some(Self::Error)
        } else if msg.starts_with("bug") {
            Some(Self::Bug)
        } else if msg.starts_with("panic") {
            Some(Self::Panic)
        } else {
            None
        };
    }
}

impl TransactionalInput {
    pub fn new_from_file(file: PathBuf, config: &CompilerSetting) -> Self {
        let code = std::fs::read_to_string(&file).unwrap();
        Self {
            code,
            config: config.clone(),
        }
    }

    pub fn new_from_str(code: &str, config: &CompilerSetting) -> Self {
        Self {
            code: code.to_string(),
            config: config.clone(),
        }
    }

    pub fn get_file_path(&self) -> (PathBuf, TempDir) {
        create_tmp_move_file(&self.code, None)
    }
}

impl TransactionalResult {
    pub fn from_run_result(res: &Result<(), Box<dyn Error>>, duration: Duration) -> Self {
        match res {
            Ok(_) => Self {
                log: "Success".to_string(),
                status: ResultStatus::Success,
                v1_chunks: vec![],
                v2_chunks: vec![],
                duration,
            },
            Err(e) => {
                let log = format!("{:?}", e);
                let (v1_log, v2_log) = Self::split_diff_log(&log);
                let v1_chunks = ResultChunk::log_to_chunck(v1_log);
                let v2_chunks = ResultChunk::log_to_chunck(v2_log);
                let status = ResultStatus::check_chunks(&v1_chunks, &v2_chunks);
                Self {
                    log,
                    v1_chunks,
                    v2_chunks,
                    status,
                    duration,
                }
            },
        }
    }

    fn split_diff_log(log: &str) -> (Vec<String>, Vec<String>) {
        let mut left = vec![];
        let mut right = vec![];
        for line in log.lines() {
            let line = line.trim();
            if line.len() < 2 {
                continue;
            }
            // split line into diff sign and content
            let (diff_sign, content) = line.trim().split_at(2);
            let content = content.trim();
            match diff_sign.trim() {
                "-" => left.push(content.to_string()),
                "+" => right.push(content.to_string()),
                "=" => {
                    left.push(content.to_string());
                    right.push(content.to_string());
                },
                _ => (),
            }
        }
        (left, right)
    }
}

impl ResultStatus {
    pub fn check_chunks(v1_chunks: &[ResultChunk], v2_chunks: &[ResultChunk]) -> Self {
        if v1_chunks.is_empty() && v2_chunks.is_empty() {
            return Self::Success;
        }
        if v1_chunks.len() != v2_chunks.len() {
            return Self::Failure;
        }
        for i in 0..v1_chunks.len() {
            if v2_chunks[i].kind == ResultChunkKind::Bug {
                return Self::Failure;
            }
            if v1_chunks[i].canonical != v2_chunks[i].canonical {
                return Self::Failure;
            }
        }
        Self::Success
    }
}

static LOCAL_PAT: Lazy<Regex> = Lazy::new(|| Regex::new(r"local\s+`[^`]+`").unwrap());

static MODULE_PAT: Lazy<Regex> = Lazy::new(|| Regex::new(r"module\s+'[^']+'").unwrap());

static TYPE_PAT: Lazy<Regex> = Lazy::new(|| Regex::new(r"type\s+`[^`]+`").unwrap());

static SOME_PAT: Lazy<Regex> = Lazy::new(|| Regex::new(r"Some\([^\)]+\)").unwrap());

static ERROR_CODE_PAT: Lazy<Regex> = Lazy::new(|| Regex::new(r"`([^`]*)`").unwrap());

impl ResultChunk {
    fn log_to_chunck(log: Vec<String>) -> Vec<ResultChunk> {
        let mut chunks = vec![];
        for line in log.into_iter() {
            if let Some(kind) = ResultChunkKind::try_from_str(&line) {
                chunks.push(ResultChunk {
                    canonical: String::new(),
                    kind,
                    lines: vec![line],
                });
            } else if let Some(last_chunk) = chunks.last_mut() {
                last_chunk.lines.push(line);
            } else {
                error!("cannot parse line: {:?}", line);
            }
        }
        chunks.retain(|e| e.kind != ResultChunkKind::Warning);
        chunks
            .iter_mut()
            .for_each(|e| e.canonical = e.get_canonicalized_msg());
        chunks
    }

    fn get_canonicalized_msg(&self) -> String {
        let top = match self.kind {
            ResultChunkKind::VMError => self.lines.get(1).unwrap().trim(),
            _ => self.lines.get(0).unwrap(),
        }
        .to_string();

        if top.contains("major_status") {
            return top
                .replace("major_status: ", "error_code: ")
                .replace(",", "");
        }
        if top.contains("bytecode verification failed") {
            if let Some(caps) = ERROR_CODE_PAT.captures(&top) {
                return format!("error_code: {}", caps.get(1).unwrap().as_str());
            }
        }

        if top.contains("mutable ownership violated")
            || top.contains("which is still mutably borrowed")
        {
            return "...cannot copy while mutably borrowed...".to_string();
        }

        if top.contains("cannot extract resource") || top.contains("function acquires global") {
            return "...cannot acquire...".to_string();
        }

        if top.contains("cannot infer type")
            || top.contains("unable to infer instantiation of type")
        {
            return "...cannot infer type...".to_string();
        }
        let replaced = LOCAL_PAT.replace_all(&top, "[some variable]").to_string();
        let replaced = MODULE_PAT
            .replace_all(&replaced, "[some module]")
            .to_string();
        let replaced = TYPE_PAT.replace_all(&replaced, "[some type]").to_string();
        let replaced = SOME_PAT.replace_all(&replaced, "[some value]").to_string();
        replaced
    }
}

impl Display for TransactionalResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "Status: {:?}", self.status)?;
        writeln!(f, "Duration: {:?}", self.duration)?;
        writeln!(f, "\nV1 output:")?;
        for chunk in self.v1_chunks.iter() {
            writeln!(f, "{}", chunk.canonical)?;
        }
        writeln!(f, "\nV2 output:")?;
        for chunk in self.v2_chunks.iter() {
            writeln!(f, "{}", chunk.canonical)?;
        }
        Ok(())
    }
}

impl Executor<TransactionalInput, TransactionalResult> for TransactionalRunner {
    fn empty_executor() -> Self {
        Self {
            saved_results: BTreeSet::new(),
        }
    }

    fn execute_one(&self, input: &TransactionalInput) -> TransactionalResult {
        let (path, dir) = input.get_file_path();

        let experiments = input.config.to_expriments();
        let vm_test_config = TestRunConfig::ComparisonV1V2 {
            language_version: LanguageVersion::V2_0,
            v2_experiments: experiments,
        };

        let prev_hook = panic::take_hook();
        panic::set_hook(Box::new(|_| {}));
        let start = Instant::now();
        let result = match panic::catch_unwind(|| {
            vm_test_harness::run_test_with_config_and_exp_suffix(vm_test_config, &path, &None)
        }) {
            Ok(res) => res,
            Err(e) => Err(anyhow::anyhow!("{:?}", e).into()),
        };
        let duration = start.elapsed();
        panic::set_hook(prev_hook);

        let output = TransactionalResult::from_run_result(&result, duration);
        dir.close().unwrap();
        output
    }

    fn save_result(&mut self, result: TransactionalResult) {
        unimplemented!()
    }

    fn should_ignore(&self, result: &TransactionalResult) -> bool {
        // TODO: implement this
        return false;
    }

    fn is_bug(&self, result: &TransactionalResult) -> bool {
        return result.status != ResultStatus::Success;
    }
}
