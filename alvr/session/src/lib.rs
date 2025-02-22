mod settings;

pub use settings::*;
pub use settings_schema;

use alvr_common::{
    anyhow::{bail, Result},
    semver::Version,
    ToAny, ALVR_VERSION,
};
use serde::{Deserialize, Serialize};
use serde_json as json;
use settings_schema::{NumberType, SchemaNode};
use std::{
    collections::{HashMap, HashSet},
    net::IpAddr,
    path::PathBuf,
};

// SessionSettings is similar to Settings but it contains every branch, even unused ones. This is
// the settings representation that the UI uses.
// 在同一crate下的settings.rs里面也有配置，但是这个包括了它，这个是UI用的
pub type SessionSettings = settings::SettingsDefault;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct DriversBackup {
    pub alvr_path: PathBuf,
    pub other_paths: Vec<PathBuf>,
}

// This structure is used to store the minimum configuration data that ALVR driver needs to
// initialize OpenVR before having the chance to communicate with a client. When a client is
// connected, a new OpenvrConfig instance is generated, then the connection is accepted only if that
// instance is equivalent to the one stored in the session, otherwise SteamVR is restarted.
// Other components (like the encoder, audio recorder) don't need this treatment and are initialized
// dynamically.
// todo: properties that can be set after the OpenVR initialization should be removed and set with
// UpdateForStream.
#[derive(Serialize, Deserialize, PartialEq, Default, Clone, Debug)]
pub struct OpenvrConfig {
    // 所有关于OpenVR的配置都在这里
    pub eye_resolution_width: u32,
    pub eye_resolution_height: u32,
    pub target_eye_resolution_width: u32,
    pub target_eye_resolution_height: u32,
    pub tracking_ref_only: bool,
    pub enable_vive_tracker_proxy: bool,
    pub aggressive_keyframe_resend: bool,
    pub adapter_index: u32,
    pub codec: u32,
    pub refresh_rate: u32,
    pub use_10bit_encoder: bool,
    pub enable_vbaq: bool,
    pub use_preproc: bool,
    pub preproc_sigma: u32,
    pub preproc_tor: u32,
    pub amd_encoder_quality_preset: u32,
    pub rate_control_mode: u32,
    pub filler_data: bool,
    pub entropy_coding: u32,
    pub force_sw_encoding: bool,
    pub sw_thread_count: u32,
    pub controller_is_tracker: bool,
    pub controllers_enabled: bool,
    pub enable_foveated_rendering: bool,
    pub foveation_center_size_x: f32,
    pub foveation_center_size_y: f32,
    pub foveation_center_shift_x: f32,
    pub foveation_center_shift_y: f32,
    pub foveation_edge_ratio_x: f32,
    pub foveation_edge_ratio_y: f32,
    pub enable_color_correction: bool,
    pub brightness: f32,
    pub contrast: f32,
    pub saturation: f32,
    pub gamma: f32,
    pub sharpening: f32,
    pub linux_async_reprojection: bool,
    pub nvenc_quality_preset: u32,
    pub nvenc_tuning_preset: u32,
    pub nvenc_multi_pass: u32,
    pub nvenc_adaptive_quantization_mode: u32,
    pub nvenc_low_delay_key_frame_scale: i64,
    pub nvenc_refresh_rate: i64,
    pub enable_intra_refresh: bool,
    pub intra_refresh_period: i64,
    pub intra_refresh_count: i64,
    pub max_num_ref_frames: i64,
    pub gop_length: i64,
    pub p_frame_strategy: i64,
    pub nvenc_rate_control_mode: i64,
    pub rc_buffer_size: i64,
    pub rc_initial_delay: i64,
    pub rc_max_bitrate: i64,
    pub rc_average_bitrate: i64,
    pub nvenc_enable_weighted_prediction: bool,
    pub capture_frame_dir: String,
    pub amd_bitrate_corruption_fix: bool,

    // these settings are not used on the C++ side, but we need them to correctly trigger a SteamVR
    // restart
    pub _controller_profile: i32,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum ConnectionState {
    Disconnected,
    Connecting,
    Connected,
    Streaming,
    Disconnecting { should_be_removed: bool },
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ClientConnectionConfig {
    pub display_name: String,        // 用户连接描述 -- 客户端名称
    pub current_ip: Option<IpAddr>,  // 用户连接描述 -- 客户端IP
    pub manual_ips: HashSet<IpAddr>, // 用户连接描述 -- 手动输入的用户列表
    pub trusted: bool,               // 用户连接描述 -- 是否信任
    pub connection_state: ConnectionState,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SessionConfig {
    pub server_version: Version,               // 连接会话描述 -- 服务器版本
    pub drivers_backup: Option<DriversBackup>, // 连接会话描述 -- 是否显示安装向导
    pub openvr_config: OpenvrConfig,           // 连接会话描述 -- OpenVR配置
    // The hashmap key is the hostname
    pub client_connections: HashMap<String, ClientConnectionConfig>, // 连接会话描述 -- 客户端连接列表
    pub session_settings: SessionSettings, // 连接会话描述 -- 会话设置(在setting)
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            // 默认构建一个会话描述。。。
            server_version: ALVR_VERSION.clone(),
            drivers_backup: None,
            openvr_config: OpenvrConfig {
                // avoid realistic resolutions, as on first start, on Linux, it
                // could trigger direct mode on an existing monitor
                eye_resolution_width: 800,
                eye_resolution_height: 900,
                target_eye_resolution_width: 800,
                target_eye_resolution_height: 900,
                adapter_index: 0,
                refresh_rate: 60,
                controllers_enabled: false,
                enable_foveated_rendering: false,
                enable_color_correction: false,
                linux_async_reprojection: false,
                capture_frame_dir: "/tmp".into(),
                ..<_>::default()
            },
            client_connections: HashMap::new(),
            session_settings: settings::session_settings_default(),
        }
    }
}

impl SessionConfig {
    // If json_value is not a valid representation of SessionConfig (because of version upgrade),
    // use some fuzzy logic to extrapolate as much information as possible.
    // Since SessionConfig cannot have a schema (because SessionSettings would need to also have a
    // schema, but it is generated out of our control), we only do basic name checking on fields and
    // deserialization will fail if the type of values does not match. Because of this,
    // `session_settings` must be handled separately to do a better job of retrieving data using the
    // settings schema.
    // 如果 json_value 不是一个有效的 SessionDesc 表示（因为版本升级），则使用一些模糊逻辑来尽可能多地推断信息。
    // 由于 SessionDesc 不能有一个模板（因为 SessionSettings 也需要有一个模式，但它是由我们无法控制的方式生成的）
    // 我只对字段做基本的名称检查，如果值的类型不匹配，反序列化将失败。因此，`session_settings` 必须单独处理，以便使用设置模式更好地检索数据。
    pub fn merge_from_json(&mut self, json_value: &json::Value) -> Result<()> {
        const SESSION_SETTINGS_STR: &str = "session_settings";

        // 尝试从json格式读取新的会话描述，如果正常读取了，直接返回
        if let Ok(session_desc) = json::from_value(json_value.clone()) {
            *self = session_desc;
            return Ok(());
        }

        // 否则，用json格式储存旧的会话描述
        // Note: unwrap is safe because current session is expected to serialize correctly
        let old_session_json = json::to_value(&self).unwrap();
        let old_session_fields = old_session_json.as_object().unwrap();

        // 最大程度推测新的会话描述
        let maybe_session_settings_json =
            json_value
                .get(SESSION_SETTINGS_STR)
                .map(|new_session_settings_json| {
                    // 利用旧的会话描述中的sessionSettings字段和新的会话描述中的sessionSettings字段，推测新的sessionSettings字段
                    extrapolate_session_settings_from_session_settings(
                        &old_session_fields[SESSION_SETTINGS_STR],
                        new_session_settings_json,
                        &Settings::schema(settings::session_settings_default()),
                    )
                });

        // 如果有新的字段，除非是sessionSettings字段，否则用新的字段替换旧的字段
        let new_fields = old_session_fields
            .iter()
            .map(|(name, json_field_value)| {
                let new_json_field_value = if name == SESSION_SETTINGS_STR {
                    // 对于sessionSettings字段，新建一个默认的sessionSettings字段
                    json::to_value(settings::session_settings_default()).unwrap()
                } else {
                    json_value.get(name).unwrap_or(json_field_value).clone()
                };
                (name.clone(), new_json_field_value)
            })
            .collect();
        // Failure to extrapolate other session_desc fields is not notified.
        let mut session_desc_mut =
            json::from_value::<SessionConfig>(json::Value::Object(new_fields)).unwrap_or_default();

        match maybe_session_settings_json
            .to_any()
            .and_then(|s| serde_json::from_value::<SessionSettings>(s).map_err(|e| e.into()))
        {
            // 如果sessionSettings字段正常读取，会添加到session_desc_mut中
            Ok(session_settings) => {
                session_desc_mut.session_settings = session_settings;
                *self = session_desc_mut;
                Ok(())
            }
            Err(e) => {
                *self = session_desc_mut;

                bail!("Error while deserializing extrapolated session settings: {e}")
            }
        }
    }

    pub fn to_settings(&self) -> Settings {
        let session_settings_json = json::to_value(&self.session_settings).unwrap();
        let schema = Settings::schema(settings::session_settings_default());

        json::from_value::<Settings>(json_session_settings_to_settings(
            &session_settings_json,
            &schema,
        ))
        .map_err(|e| dbg!(e))
        .unwrap()
    }
}

// Current data extrapolation strategy: match both field name and value type exactly.
// Integer bounds are not validated, if they do not match the schema, deserialization will fail and
// all data is lost.
// Future strategies: check if value respects schema constraints, fuzzy field name matching, accept
// integer to float and float to integer, tree traversal.
fn extrapolate_session_settings_from_session_settings(
    old_session_settings: &json::Value,
    new_session_settings: &json::Value,
    schema: &SchemaNode,
) -> json::Value {
    match schema {
        SchemaNode::Section(entries) => json::Value::Object({
            let mut entries: json::Map<String, json::Value> = entries
                .iter()
                .map(|named_entry| {
                    let value_json =
                        if let Some(new_value_json) = new_session_settings.get(&named_entry.name) {
                            extrapolate_session_settings_from_session_settings(
                                &old_session_settings[&named_entry.name],
                                new_value_json,
                                &named_entry.content,
                            )
                        } else {
                            old_session_settings[&named_entry.name].clone()
                        };
                    (named_entry.name.clone(), value_json)
                })
                .collect();

            let collapsed_state = new_session_settings
                .get("gui_collapsed")
                .and_then(|val| val.as_bool())
                .unwrap_or_else(|| {
                    old_session_settings
                        .get("gui_collapsed")
                        .unwrap()
                        .as_bool()
                        .unwrap()
                });
            entries.insert("gui_collapsed".into(), json::Value::Bool(collapsed_state));

            entries
        }),

        SchemaNode::Choice { variants, .. } => {
            let variant_json = new_session_settings
                .get("variant")
                .cloned()
                .filter(|new_variant_json| {
                    new_variant_json
                        .as_str()
                        .map(|variant_str| {
                            variants
                                .iter()
                                .any(|named_entry| variant_str == named_entry.name)
                        })
                        .unwrap_or(false)
                })
                .unwrap_or_else(|| old_session_settings["variant"].clone());

            let mut fields: json::Map<_, _> = variants
                .iter()
                .filter_map(|named_entry| {
                    named_entry.content.as_ref().map(|data_schema| {
                        let value_json = if let Some(new_value_json) =
                            new_session_settings.get(&named_entry.name)
                        {
                            extrapolate_session_settings_from_session_settings(
                                &old_session_settings[&named_entry.name],
                                new_value_json,
                                data_schema,
                            )
                        } else {
                            old_session_settings[&named_entry.name].clone()
                        };
                        (named_entry.name.clone(), value_json)
                    })
                })
                .collect();
            fields.insert("variant".into(), variant_json);

            json::Value::Object(fields)
        }

        SchemaNode::Optional { content, .. } => {
            let set_json = new_session_settings
                .get("set")
                .cloned()
                .filter(|new_set_json| new_set_json.is_boolean())
                .unwrap_or_else(|| old_session_settings["set"].clone());

            let content_json = new_session_settings
                .get("content")
                .map(|new_content_json| {
                    extrapolate_session_settings_from_session_settings(
                        &old_session_settings["content"],
                        new_content_json,
                        content,
                    )
                })
                .unwrap_or_else(|| old_session_settings["content"].clone());

            json::json!({
                "set": set_json,
                "content": content_json
            })
        }

        SchemaNode::Switch { content, .. } => {
            let enabled_json = new_session_settings
                .get("enabled")
                .cloned()
                .filter(|new_enabled_json| new_enabled_json.is_boolean())
                .unwrap_or_else(|| old_session_settings["enabled"].clone());

            let content_json = new_session_settings
                .get("content")
                .map(|new_content_json| {
                    extrapolate_session_settings_from_session_settings(
                        &old_session_settings["content"],
                        new_content_json,
                        content,
                    )
                })
                .unwrap_or_else(|| old_session_settings["content"].clone());

            json::json!({
                "enabled": enabled_json,
                "content": content_json
            })
        }

        SchemaNode::Boolean { .. } => {
            if new_session_settings.is_boolean() {
                new_session_settings.clone()
            } else {
                old_session_settings.clone()
            }
        }

        SchemaNode::Number { ty, .. } => {
            if let Some(value) = new_session_settings.as_f64() {
                match ty {
                    NumberType::UnsignedInteger => json::Value::from(value.abs() as u64),
                    NumberType::SignedInteger => json::Value::from(value as i64),
                    NumberType::Float => new_session_settings.clone(),
                }
            } else {
                old_session_settings.clone()
            }
        }

        SchemaNode::Text { .. } => {
            if new_session_settings.is_string() {
                new_session_settings.clone()
            } else {
                old_session_settings.clone()
            }
        }

        SchemaNode::Array(array_schema) => {
            let array_vec = (0..array_schema.len())
                .map(|idx| {
                    new_session_settings
                        .get(idx)
                        .cloned()
                        .unwrap_or_else(|| old_session_settings[idx].clone())
                })
                .collect();
            json::Value::Array(array_vec)
        }

        SchemaNode::Vector {
            default_element, ..
        } => {
            let element_json = new_session_settings
                .get("element")
                .map(|new_element_json| {
                    extrapolate_session_settings_from_session_settings(
                        &old_session_settings["element"],
                        new_element_json,
                        default_element,
                    )
                })
                .unwrap_or_else(|| old_session_settings["element"].clone());

            // todo: content field cannot be properly validated until I implement plain settings
            // validation (not to be confused with session/session_settings validation). Any
            // problem inside this new_session_settings content will result in the loss all data in the new
            // session_settings.
            let content_json = new_session_settings
                .get("content")
                .cloned()
                .unwrap_or_else(|| old_session_settings["content"].clone());

            json::json!({
                "element": element_json,
                "content": content_json
            })
        }

        SchemaNode::Dictionary { default_value, .. } => {
            let key_json = new_session_settings
                .get("key")
                .cloned()
                .filter(|new_key| new_key.is_string())
                .unwrap_or_else(|| old_session_settings["key"].clone());

            let value_json = new_session_settings
                .get("value")
                .map(|new_value_json| {
                    extrapolate_session_settings_from_session_settings(
                        &old_session_settings["value"],
                        new_value_json,
                        default_value,
                    )
                })
                .unwrap_or_else(|| old_session_settings["value"].clone());

            // todo: validate content using settings validation
            let content_json = new_session_settings
                .get("content")
                .cloned()
                .unwrap_or_else(|| old_session_settings["content"].clone());

            json::json!({
                "key": key_json,
                "value": value_json,
                "content": content_json
            })
        }
        _ => unreachable!(),
    }
}

// session_settings does not get validated here, it must be already valid
fn json_session_settings_to_settings(
    session_settings: &json::Value,
    schema: &SchemaNode,
) -> json::Value {
    match schema {
        SchemaNode::Section(entries) => json::Value::Object(
            entries
                .iter()
                .map(|named_entry| {
                    (
                        named_entry.name.clone(),
                        json_session_settings_to_settings(
                            &session_settings[&named_entry.name],
                            &named_entry.content,
                        ),
                    )
                })
                .collect(),
        ),

        SchemaNode::Choice { variants, .. } => {
            let variant = session_settings["variant"].as_str().unwrap();
            let maybe_content = variants
                .iter()
                .find(|named_entry| named_entry.name == variant)
                .and_then(|named_entry| named_entry.content.as_ref())
                .map(|data_schema| {
                    json_session_settings_to_settings(&session_settings[variant], data_schema)
                });
            if let Some(content) = maybe_content {
                json::json!({ variant: content })
            } else {
                json::Value::String(variant.to_owned())
            }
        }

        SchemaNode::Optional { content, .. } => {
            if session_settings["set"].as_bool().unwrap() {
                json_session_settings_to_settings(&session_settings["content"], content)
            } else {
                json::Value::Null
            }
        }

        SchemaNode::Switch { content, .. } => {
            if session_settings["enabled"].as_bool().unwrap() {
                let content =
                    json_session_settings_to_settings(&session_settings["content"], content);

                json::json!({ "Enabled": content })
            } else {
                json::Value::String("Disabled".into())
            }
        }

        SchemaNode::Boolean { .. } | SchemaNode::Number { .. } | SchemaNode::Text { .. } => {
            session_settings.clone()
        }

        SchemaNode::Array(array_schema) => json::Value::Array(
            array_schema
                .iter()
                .enumerate()
                .map(|(idx, element_schema)| {
                    json_session_settings_to_settings(&session_settings[idx], element_schema)
                })
                .collect(),
        ),

        SchemaNode::Vector {
            default_element, ..
        } => json::to_value(
            session_settings["content"]
                .as_array()
                .unwrap()
                .iter()
                .map(|element_json| {
                    json_session_settings_to_settings(element_json, default_element)
                })
                .collect::<Vec<_>>(),
        )
        .unwrap(),

        SchemaNode::Dictionary { default_value, .. } => json::to_value(
            json::from_value::<Vec<(String, json::Value)>>(session_settings["content"].clone())
                .unwrap()
                .into_iter()
                .map(|(key, value_json)| {
                    (
                        key,
                        json_session_settings_to_settings(&value_json, default_value),
                    )
                })
                .collect::<Vec<_>>(),
        )
        .unwrap(),
        _ => unreachable!(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_manual_session_to_settings() {
        let default = session_settings_default();
        let settings_schema = json::to_value(&default).unwrap();
        let schema = Settings::schema(default);

        let _settings = json::from_value::<Settings>(json_session_settings_to_settings(
            &settings_schema,
            &schema,
        ))
        .err();
    }

    #[test]
    fn test_session_to_settings() {
        let _settings = SessionConfig::default().to_settings();
    }

    #[test]
    fn test_session_extrapolation_trivial() {
        SessionConfig::default()
            .merge_from_json(&json::to_value(SessionConfig::default()).unwrap())
            .unwrap();
    }

    #[test]
    fn test_session_extrapolation_diff() {
        let input_json_string = r#"{
            "session_settings": {
              "gui_collapsed": false,
              "fjdshfks": false,
              "video": {
                "preferred_fps": 60.0
              },
              "headset": {
                "gui_collapsed": false,
                "controllers": {
                  "enabled": false
                }
              }
            }
          }"#;

        SessionConfig::default()
            .merge_from_json(&json::from_str(input_json_string).unwrap())
            .unwrap();
    }
}
