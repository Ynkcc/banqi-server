use crate::app::sync::lock;
use banqi_core::core::env::config::GameConfig;
use banqi_engine::inference::ModelCache;
#[cfg(feature = "onnx")]
use banqi_engine::inference::OnnxModelCache;
use banqi_engine::nnue::GraphEvaluator;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
pub fn scan_model_files(dir: &Path, extensions: &[&str], max_depth: usize) -> Vec<PathBuf> {
    let mut out = Vec::new();
    collect_model_files(dir, 0, extensions, max_depth, &mut out);
    out.sort();
    out
}
fn collect_model_files(
    dir: &Path,
    depth: usize,
    extensions: &[&str],
    max_depth: usize,
    out: &mut Vec<PathBuf>,
) {
    if depth > max_depth {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        if depth == 0 {
            tracing::warn!("模型目录不可读 ({}), 所有变体 AI 均不可用", dir.display());
        }
        return;
    };
    for e in entries.flatten() {
        let path = e.path();
        match e.file_type() {
            Ok(ft) if ft.is_dir() => {
                collect_model_files(&path, depth + 1, extensions, max_depth, out)
            }
            Ok(ft)
                if ft.is_file()
                    && path
                        .extension()
                        .and_then(|x| x.to_str())
                        .is_some_and(|x| extensions.contains(&x)) =>
            {
                out.push(path);
            }
            _ => {}
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EngineKind {
    MctsOnnx,
    Nnue,
    MctsDL,
}
impl EngineKind {
    pub fn from_model_file(file: &str) -> Self {
        use banqi_engine::inference::ModelFormat;
        match ModelFormat::from_path(file) {
            Some(ModelFormat::Nnue) => Self::Nnue,
            Some(ModelFormat::Torch) => Self::MctsDL,

            _ => Self::MctsOnnx,
        }
    }
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::MctsOnnx => "mcts-onnx",
            Self::Nnue => "nnue",
            Self::MctsDL => "mcts-dl",
        }
    }
}
#[cfg(feature = "torch")]
pub(crate) const SCAN_EXTENSIONS: &[&str] = &["pt", "onnx", "nnue"];
#[cfg(not(feature = "torch"))]
pub(crate) const SCAN_EXTENSIONS: &[&str] = &["onnx", "nnue"];
pub struct ModelPool {
    files: Mutex<Vec<String>>,
    #[cfg(feature = "onnx")]
    onnx_cache: OnnxModelCache,
    #[cfg(feature = "torch")]
    loaded_torch: ModelCache<banqi_engine::engine::ModelWrapper>,
    loaded_nnue: ModelCache<GraphEvaluator>,
    models_dir: PathBuf,
    #[cfg(feature = "onnx")]
    sessions: usize,
    #[cfg(feature = "onnx")]
    device: String,
    #[cfg(feature = "onnx")]
    nn_cache_entries: usize,
}
impl ModelPool {
    pub fn new(models_dir: &Path, sessions: usize, device: &str, nn_cache_entries: usize) -> Self {
        #[cfg(not(feature = "onnx"))]
        let _ = (sessions, device, nn_cache_entries);
        let models = Self {
            files: Mutex::new(Vec::new()),
            #[cfg(feature = "onnx")]
            onnx_cache: OnnxModelCache::new(device, sessions, nn_cache_entries),
            #[cfg(feature = "torch")]
            loaded_torch: ModelCache::new(),
            loaded_nnue: ModelCache::new(),
            models_dir: models_dir.to_path_buf(),
            #[cfg(feature = "onnx")]
            sessions,
            #[cfg(feature = "onnx")]
            device: device.to_string(),
            #[cfg(feature = "onnx")]
            nn_cache_entries,
        };
        models.rescan();
        models
    }
    fn rescan(&self) {
        let paths = scan_model_files(&self.models_dir, SCAN_EXTENSIONS, 4);
        let files: Vec<String> = paths
            .into_iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect();
        let n = files.len();
        *lock(&self.files) = files;
        if n == 0 {
            tracing::warn!(
                "模型目录未找到任何模型文件 {:?} ({}，含子目录)",
                SCAN_EXTENSIONS,
                self.models_dir.display()
            );
        } else {
            tracing::info!(
                "已扫描到 {n} 个模型文件（{}），随用随加载：对局绑定的文件首次使用时才加载权重",
                self.models_dir.display()
            );
        }
    }
    pub fn files(&self) -> Vec<String> {
        self.rescan();
        lock(&self.files).clone()
    }
    #[cfg(feature = "onnx")]
    pub fn acquire_onnx(
        &self,
        file: &str,
    ) -> Result<Arc<banqi_engine::inference::onnx::OnnxModel>, String> {
        if self.onnx_cache.get(file).is_none() {
            tracing::info!(
                "首次使用，加载 ONNX 模型: {file}（会话池 {}，device {}，nncache {}）",
                self.sessions,
                self.device,
                if self.nn_cache_entries > 0 {
                    format!("开启（≤{} 条）", self.nn_cache_entries)
                } else {
                    "关闭".to_string()
                }
            );
        }
        self.onnx_cache.acquire(file)
    }
    #[cfg(not(feature = "onnx"))]
    #[allow(dead_code)]
    pub fn acquire_onnx(&self, _file: &str) -> Result<std::convert::Infallible, String> {
        Err("编译未启用 onnx 推理，ONNX 模型不可用（.nnue 对手仍可用）".to_string())
    }
    #[cfg(feature = "torch")]
    pub fn acquire_torch(
        &self,
        file: &str,
    ) -> Result<Arc<banqi_engine::engine::ModelWrapper>, String> {
        self.loaded_torch.get_or_load(file, |p| {
            tracing::info!("首次使用，加载 TorchScript 模型: {p}");
            Ok(Arc::new(
                banqi_engine::engine::ModelWrapper::load_from_file(p)?,
            ))
        })
    }
    #[cfg(not(feature = "torch"))]
    #[allow(dead_code)]
    pub fn acquire_torch(&self, _file: &str) -> Result<std::convert::Infallible, String> {
        Err(
            "编译未启用 torch 推理，TorchScript (.pt) 模型不可用（.onnx / .nnue 对手仍可用）"
                .to_string(),
        )
    }
    pub fn acquire_nnue(&self, file: &str) -> Result<Arc<GraphEvaluator>, String> {
        self.loaded_nnue.get_or_load(file, |p| {
            tracing::info!("首次使用，加载 NNUE 模型: {p}");
            Ok(Arc::new(GraphEvaluator::load_from_file(p)?))
        })
    }
    pub fn acquire_nnue_validated(
        &self,
        file: &str,
        config: &GameConfig,
    ) -> Result<Arc<GraphEvaluator>, String> {
        let evaluator = self.acquire_nnue(file)?;
        evaluator
            .validate_layout(config)
            .map_err(|e| format!(".nnue 权重与变体 {} 不匹配: {e}", config.variant.as_str()))?;
        Ok(evaluator)
    }
    pub fn loaded_files(&self) -> Vec<(String, bool)> {
        self.files();
        let loaded_keys: std::collections::HashSet<String> = {
            #[cfg_attr(not(any(feature = "onnx", feature = "torch")), allow(unused_mut))]
            let mut set: std::collections::HashSet<String> =
                self.loaded_nnue.loaded_keys().into_iter().collect();
            #[cfg(feature = "onnx")]
            set.extend(self.onnx_cache.loaded_keys());
            #[cfg(feature = "torch")]
            set.extend(self.loaded_torch.loaded_keys());
            set
        };
        lock(&self.files)
            .iter()
            .map(|f| (f.clone(), loaded_keys.contains(f)))
            .collect()
    }
}
#[cfg(feature = "onnx")]
pub fn check_onnx_model_matches<G: banqi_core::core::env::GameEnv>(
    m: &banqi_engine::inference::onnx::OnnxModel,
    game: &G,
) -> Result<(), String> {
    let obs = game.get_resnet_state();
    let (ch, rows, cols) = (
        obs.board.shape()[0],
        obs.board.shape()[1],
        obs.board.shape()[2],
    );
    let scalars = obs.scalars.len();
    m.validate_layout(ch, rows, cols, scalars).map_err(|e| {
        format!("模型与当前变体不匹配（当前观测 {ch}x{rows}x{cols} / 标量 {scalars}）: {e}")
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scan_collects_matching_extensions_recursively_and_sorts() {
        let base = std::env::temp_dir().join(format!("banqi-app-scan-test-{}", std::process::id()));
        let sub = base.join("nested");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(base.join("b.onnx"), b"x").unwrap();
        std::fs::write(base.join("a.nnue"), b"x").unwrap();
        std::fs::write(sub.join("deep.pt"), b"x").unwrap();
        std::fs::write(base.join("ignored.txt"), b"x").unwrap();
        let mut files = scan_model_files(&base, &["onnx", "nnue", "pt"], 4)
            .into_iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
            .collect::<Vec<_>>();
        files.sort();
        assert_eq!(files, vec!["a.nnue", "b.onnx", "deep.pt"]);
        std::fs::remove_dir_all(&base).ok();
    }
    #[test]
    fn scan_respects_max_depth() {
        let base =
            std::env::temp_dir().join(format!("banqi-app-depth-test-{}", std::process::id()));
        let deep = base.join("l1/l2/l3/l4/l5");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(deep.join("far.onnx"), b"x").unwrap();
        std::fs::write(base.join("l1/near.onnx"), b"x").unwrap();
        let names: Vec<String> = scan_model_files(&base, &["onnx"], 4)
            .into_iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert_eq!(names, vec!["near.onnx"]);
        std::fs::remove_dir_all(&base).ok();
    }
    #[test]
    fn scan_missing_dir_is_empty() {
        let missing = std::env::temp_dir().join("banqi-app-no-such-dir");
        assert!(scan_model_files(&missing, &["onnx"], 4).is_empty());
    }
    #[test]
    fn engine_kind_by_extension() {
        assert_eq!(EngineKind::from_model_file("a.nnue"), EngineKind::Nnue);
        assert_eq!(EngineKind::from_model_file("a.NNUE"), EngineKind::Nnue);
        assert_eq!(EngineKind::from_model_file("a.onnx"), EngineKind::MctsOnnx);
        assert_eq!(EngineKind::from_model_file("a.PT"), EngineKind::MctsDL);
        assert_eq!(EngineKind::from_model_file("a.pt"), EngineKind::MctsDL);
        assert_eq!(EngineKind::Nnue.as_str(), "nnue");
        assert_eq!(EngineKind::MctsOnnx.as_str(), "mcts-onnx");
        assert_eq!(EngineKind::MctsDL.as_str(), "mcts-dl");
    }
    #[test]
    fn pool_scans_and_reports_files() {
        let base = std::env::temp_dir().join(format!("banqi-app-pool-test-{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        std::fs::write(base.join("4x8.nnue"), b"x").unwrap();
        let pool = ModelPool::new(&base, 0, "cpu", 0);
        assert_eq!(
            pool.files(),
            vec![base.join("4x8.nnue").to_string_lossy().to_string()]
        );
        let loaded = pool.loaded_files();
        assert_eq!(loaded.len(), 1);
        assert!(!loaded[0].1, "未使用过的模型不应已加载");
        std::fs::remove_dir_all(&base).ok();
    }
}
