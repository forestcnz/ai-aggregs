use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde::Serialize;

use crate::config::types::{Config, Protocol};
use crate::gateway::provider::{KeyStatus, Provider};
use crate::infra::log_bridge::LogLevelSetter;

pub struct ServerHandle {
    pub shutdown_tx: tokio::sync::oneshot::Sender<()>,
    pub join: tauri::async_runtime::JoinHandle<()>,
}

pub struct AppCtrl {
    pub config: Mutex<Config>,
    pub db: Arc<Mutex<rusqlite::Connection>>,
    pub server: Mutex<Option<ServerHandle>>,
    pub listen_addr: Mutex<String>,
    pub providers: Mutex<Vec<Arc<Provider>>>,
    pub log_level_setter: LogLevelSetter,
    /// 别名 → 上次成功响应的实际模型（内存记录，跨网关重建保留，进程退出即失）
    pub last_model: Arc<Mutex<HashMap<String, String>>>,
}

pub struct TrayItems {
    pub status: tauri::menu::MenuItem<tauri::Wry>,
    pub toggle_gw: tauri::menu::MenuItem<tauri::Wry>,
}

#[derive(Serialize)]
pub struct GatewayStatus {
    pub running: bool,
    pub listen_addr: String,
}

#[derive(Serialize)]
pub struct ProviderRuntime {
    pub name: String,
    pub enabled: bool,
    pub protocol: String,
    pub base_url: String,
    pub models: Vec<String>,
    pub keys: Vec<KeyStatus>,
}

#[derive(Clone)]
pub struct Consumer {
    pub api_keys: Vec<String>,
    pub models: Vec<String>,
}

impl Consumer {
    pub fn check_key(&self, presented: &str) -> bool {
        if self.api_keys.is_empty() {
            // 未配置 consumer key：保持向后兼容（本机任意进程可调用）
            // 但这是不安全的状态，启动时会打 warning 提示用户配置
            return true;
        }
        // 常量时间比较，缓解 timing attack（不提前短路）
        self.api_keys
            .iter()
            .any(|k| constant_time_eq::constant_time_eq(k.as_bytes(), presented.as_bytes()))
    }
}

#[derive(Clone)]
pub struct AppState {
    pub consumer: Consumer,
    pub providers: Arc<Vec<Arc<Provider>>>,
    pub model_map: Arc<HashMap<String, Vec<usize>>>,
    /// 别名 → 实际后端模型池（仅含 enabled 的映射）
    pub model_aliases: Arc<HashMap<String, Vec<String>>>,
    pub db: Arc<Mutex<rusqlite::Connection>>,
    /// model -> 上次成功的 provider id，下次路由时优先
    pub last_provider: Arc<Mutex<HashMap<String, i64>>>,
    /// 别名 -> 上次成功的实际模型（与 AppCtrl 共享同一份内存记录）
    pub last_model: Arc<Mutex<HashMap<String, String>>>,
}

impl AppState {
    pub fn build(
        cfg: &Config,
        providers: Vec<Arc<Provider>>,
        db: Arc<Mutex<rusqlite::Connection>>,
        last_model: Arc<Mutex<HashMap<String, String>>>,
    ) -> anyhow::Result<Self> {
        let mut map: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, p) in providers.iter().enumerate() {
            for m in &p.models {
                map.entry(m.clone()).or_default().push(i);
            }
        }
        let mut aliases: HashMap<String, Vec<String>> = HashMap::new();
        for m in &cfg.model_mappings {
            if !m.enabled {
                continue;
            }
            let alias = m.alias.trim().to_string();
            let pool: Vec<String> = m
                .models
                .iter()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            if !alias.is_empty() && !pool.is_empty() {
                aliases.insert(alias, pool);
            }
        }
        Ok(Self {
            consumer: Consumer {
                api_keys: cfg.consumer.api_keys.clone(),
                models: cfg.consumer.models.clone(),
            },
            providers: Arc::new(providers),
            model_map: Arc::new(map),
            model_aliases: Arc::new(aliases),
            db,
            last_provider: Arc::new(Mutex::new(HashMap::new())),
            last_model,
        })
    }

    /// 路由：返回 (provider, 实际模型) 候选序列。
    /// 若 `requested` 是已定义别名，展开为其实际后端模型池；否则池就是 [requested]。
    /// 池内模型按配置顺序尝试，每个模型内沿用原四级优先（协议+上次使用 > 协议 > 上次 > 其他）。
    pub fn route(
        &self,
        requested: &str,
        c_proto: Protocol,
    ) -> Option<Vec<(Arc<Provider>, String)>> {
        let mut pool: Vec<String> = match self.model_aliases.get(requested) {
            Some(p) if !p.is_empty() => p.clone(),
            _ => vec![requested.to_string()],
        };
        // 上次成功的实际模型优先（仅别名池有意义：把命中的模型提到池首）
        if let Some(lm) = self.last_model.lock().unwrap().get(requested).cloned() {
            if let Some(pos) = pool.iter().position(|m| m == &lm) {
                let m = pool.remove(pos);
                pool.insert(0, m);
            }
        }
        let last_id = self.last_provider.lock().unwrap().get(requested).copied();

        let mut out: Vec<(Arc<Provider>, String)> = Vec::new();
        for actual in &pool {
            let Some(idxs) = self.model_map.get(actual) else {
                continue;
            };
            // 四级优先：协议匹配+上次使用 > 协议匹配+其他 > 协议不匹配+上次使用 > 协议不匹配+其他
            let mut a = Vec::new(); // 协议匹配 + 上次使用
            let mut b = Vec::new(); // 协议匹配 + 其他
            let mut c = Vec::new(); // 协议不匹配 + 上次使用
            let mut d = Vec::new(); // 协议不匹配 + 其他
            for &i in idxs {
                let proto_match = self.providers[i].protocol == c_proto;
                let is_last = last_id == Some(self.providers[i].id);
                match (proto_match, is_last) {
                    (true, true) => a.push(i),
                    (true, false) => b.push(i),
                    (false, true) => c.push(i),
                    (false, false) => d.push(i),
                }
            }
            let mut ordered = a;
            ordered.extend(b);
            ordered.extend(c);
            ordered.extend(d);
            for i in ordered {
                out.push((self.providers[i].clone(), actual.clone()));
            }
        }
        if out.is_empty() {
            None
        } else {
            Some(out)
        }
    }
}

// ===================== 用量统计 IPC 返回类型 =====================

/// 单个模型的聚合用量
#[derive(Serialize)]
pub struct UsageModelRow {
    pub model: String,
    pub requests: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
}

/// 用量统计汇总（含各模型明细 + 总计）
#[derive(Serialize)]
pub struct UsageSummary {
    pub models: Vec<UsageModelRow>,
    pub total_requests: u64,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub total_tokens: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::types::{
        ApiKeyEntry, ConsumerConfig, LogConfig, ModelMapping, ProviderConfig,
    };
    use crate::gateway::provider::Provider;

    fn provider(id: i64, name: &str, proto: Protocol, models: &[&str]) -> Arc<Provider> {
        let pc = ProviderConfig {
            id,
            name: name.into(),
            protocol: proto,
            base_url: "https://example.com".into(),
            api_keys: vec![ApiKeyEntry::Plain("sk-test".into())],
            models: models.iter().map(|m| (*m).to_string()).collect(),
            timeout_secs: 10,
            extra_headers: HashMap::new(),
            enabled: true,
            reasoning_effort: None,
        };
        Arc::new(Provider::new(&pc, 60).unwrap())
    }

    fn state_with(providers: Vec<Arc<Provider>>, mappings: Vec<ModelMapping>) -> AppState {
        let cfg = Config {
            listen: "127.0.0.1:8849".into(),
            providers: vec![],
            consumer: ConsumerConfig {
                api_keys: vec![],
                models: vec![],
            },
            log: LogConfig {
                level: "info".into(),
            },
            key_blacklist_secs: 60,
            auto_start_gateway: false,
            model_mappings: mappings,
        };
        let db = Arc::new(Mutex::new(rusqlite::Connection::open_in_memory().unwrap()));
        let last_model = Arc::new(Mutex::new(HashMap::new()));
        AppState::build(&cfg, providers, db, last_model).unwrap()
    }

    fn models_of(res: &[(Arc<Provider>, String)]) -> Vec<&str> {
        res.iter().map(|(_, m)| m.as_str()).collect()
    }

    #[test]
    fn route_alias_expands_to_pool_in_order() {
        // p1(chat): b,c   p2(chat): d  —— 别名 A1 → [b,c,d]
        let p1 = provider(1, "p1", Protocol::Chat, &["b", "c"]);
        let p2 = provider(2, "p2", Protocol::Chat, &["d"]);
        let mappings = vec![ModelMapping {
            alias: "A1".into(),
            models: vec!["b".into(), "c".into(), "d".into()],
            enabled: true,
        }];
        let st = state_with(vec![p1, p2], mappings);

        let res = st.route("A1", Protocol::Chat).unwrap();
        assert_eq!(models_of(&res), vec!["b", "c", "d"]);
        assert_eq!(res[0].0.name, "p1"); // b,c 同属 p1
        assert_eq!(res[2].0.name, "p2"); // d 属 p2
    }

    #[test]
    fn route_non_alias_is_direct_model() {
        let p1 = provider(1, "p1", Protocol::Chat, &["b"]);
        let st = state_with(vec![p1], vec![]);
        let res = st.route("b", Protocol::Chat).unwrap();
        assert_eq!(res.len(), 1);
        assert_eq!(res[0].1, "b");
    }

    #[test]
    fn route_alias_shadows_same_named_model() {
        // 别名 b 与真实模型 b 同名 → 别名优先，展开为池 [c,d]，不含 b 自身
        let p1 = provider(1, "p1", Protocol::Chat, &["b", "c", "d"]);
        let mappings = vec![ModelMapping {
            alias: "b".into(),
            models: vec!["c".into(), "d".into()],
            enabled: true,
        }];
        let st = state_with(vec![p1], mappings);
        let res = st.route("b", Protocol::Chat).unwrap();
        assert_eq!(models_of(&res), vec!["c", "d"]);
    }

    #[test]
    fn route_unknown_model_returns_none() {
        let p1 = provider(1, "p1", Protocol::Chat, &["b"]);
        let st = state_with(vec![p1], vec![]);
        assert!(st.route("nope", Protocol::Chat).is_none());
    }

    #[test]
    fn route_disabled_alias_not_resolved() {
        let p1 = provider(1, "p1", Protocol::Chat, &["b"]);
        let mappings = vec![ModelMapping {
            alias: "A1".into(),
            models: vec!["b".into()],
            enabled: false,
        }];
        let st = state_with(vec![p1], mappings);
        // 别名禁用 → 不展开；A1 又非真实模型 → 无候选
        assert!(st.route("A1", Protocol::Chat).is_none());
    }

    #[test]
    fn route_pool_skips_nonexistent_models() {
        let p1 = provider(1, "p1", Protocol::Chat, &["b"]);
        let mappings = vec![ModelMapping {
            alias: "A1".into(),
            models: vec!["missing".into(), "b".into()],
            enabled: true,
        }];
        let st = state_with(vec![p1], mappings);
        let res = st.route("A1", Protocol::Chat).unwrap();
        assert_eq!(models_of(&res), vec!["b"]);
    }

    #[test]
    fn route_pool_prioritizes_last_success_model() {
        let p1 = provider(1, "p1", Protocol::Chat, &["b", "c", "d"]);
        let mappings = vec![ModelMapping {
            alias: "A1".into(),
            models: vec!["b".into(), "c".into(), "d".into()],
            enabled: true,
        }];
        let st = state_with(vec![p1], mappings);
        // 模拟上次成功落在 c → 应被提到池首
        st.last_model
            .lock()
            .unwrap()
            .insert("A1".into(), "c".into());
        let res = st.route("A1", Protocol::Chat).unwrap();
        assert_eq!(models_of(&res), vec!["c", "b", "d"]);
    }

    #[test]
    fn route_last_success_ignored_if_no_longer_in_pool() {
        // 池里只剩 b,d（c 已被用户移除），但 last_model 仍记着 c
        let p1 = provider(1, "p1", Protocol::Chat, &["b", "d"]);
        let mappings = vec![ModelMapping {
            alias: "A1".into(),
            models: vec!["b".into(), "d".into()],
            enabled: true,
        }];
        let st = state_with(vec![p1], mappings);
        st.last_model
            .lock()
            .unwrap()
            .insert("A1".into(), "c".into());
        // 前提不成立 → 不重排，保持配置顺序
        let res = st.route("A1", Protocol::Chat).unwrap();
        assert_eq!(models_of(&res), vec!["b", "d"]);
    }
}
