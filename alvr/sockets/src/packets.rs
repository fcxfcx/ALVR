use alvr_common::{
    glam::{Quat, UVec2, Vec2, Vec3},
    Fov, LogSeverity,
};
use alvr_events::{ButtonValue, LogEvent};
use alvr_session::SessionDesc;
use serde::{Deserialize, Serialize};
use std::{
    fmt::{self, Debug},
    net::IpAddr,
    time::Duration,
};

pub const TRACKING: u16 = 0;
pub const HAPTICS: u16 = 1;
pub const AUDIO: u16 = 2;
pub const VIDEO: u16 = 3;
pub const STATISTICS: u16 = 4;

#[derive(Serialize, Deserialize, Clone)]
pub struct VideoStreamingCapabilities {
    pub default_view_resolution: UVec2,    //默认分辨率
    pub supported_refresh_rates: Vec<f32>, //支持的刷新率
    pub microphone_sample_rate: u32,       //麦克风采样率
}

#[derive(Serialize, Deserialize)]
pub enum ClientConnectionResult {
    // 客户端连接的结果，分为接受和待机
    ConnectionAccepted {
        display_name: String,
        server_ip: IpAddr,
        streaming_capabilities: Option<VideoStreamingCapabilities>,
    },
    ClientStandby,
}

#[derive(Serialize, Deserialize)]
pub struct StreamConfigPacket {
    // 数据流配置包，包括会话描述，分辨率，帧率，采样率
    pub session_desc: String, // transfer session as string to allow for extrapolation
    pub view_resolution: UVec2,
    pub fps: f32,
    pub game_audio_sample_rate: u32,
}

#[derive(Serialize, Deserialize)]
pub enum ServerControlPacket {
    // 用于表示服务器控制的数据包，列出了一些包类型
    StartStream,
    InitializeDecoder { config_buffer: Vec<u8> },
    Restarting,
    KeepAlive,
    ServerPredictionAverage(Duration),
    Reserved(String),
    ReservedBuffer(Vec<u8>),
}

#[derive(Serialize, Deserialize, Clone)]
pub struct ViewsConfig {
    // Note: the head-to-eye transform is always a translation along the x axis
    pub ipd_m: f32,    // 瞳距
    pub fov: [Fov; 2], // 视场角
}

#[derive(Serialize, Deserialize, Clone)]
pub struct BatteryPacket {
    pub device_id: u64,   // 电池设备id
    pub gauge_value: f32, // 电量，range [0, 1]
    pub is_plugged: bool, // 是否充电
}

#[derive(Serialize, Deserialize)]
pub enum ClientControlPacket {
    // 客户端控制包，包括一些包类型
    PlayspaceSync(Option<Vec2>),
    RequestIdr,
    KeepAlive,
    StreamReady,
    ViewsConfig(ViewsConfig),
    Battery(BatteryPacket),
    VideoErrorReport, // legacy
    Button { path_id: u64, value: ButtonValue },
    ActiveInteractionProfile { device_id: u64, profile_id: u64 },
    Log { level: LogSeverity, message: String },
    Reserved(String),
    ReservedBuffer(Vec<u8>),
}

#[derive(Serialize, Deserialize, Clone, Copy, Default, Debug)]
pub struct Pose {
    pub orientation: Quat, //空间位置 -- 旋转
    pub position: Vec3,    //空间位置 -- 位移
}

#[derive(Serialize, Deserialize, Clone, Copy, Default, Debug)]
pub struct DeviceMotion {
    pub pose: Pose,             // 姿态信息 -- 位置（旋转+位移）
    pub linear_velocity: Vec3,  // 姿态信息 -- 线速度
    pub angular_velocity: Vec3, // 姿态信息 -- 角速度
}

#[derive(Serialize, Deserialize)]
pub struct Tracking {
    pub target_timestamp: Duration,               // 追踪信息--目标时间戳
    pub device_motions: Vec<(u64, DeviceMotion)>, // 追踪信息--设备运动
    pub left_hand_skeleton: Option<[Pose; 26]>,   // 追踪信息--左手骨架
    pub right_hand_skeleton: Option<[Pose; 26]>,  // 追踪信息--右手骨架
}

#[derive(Serialize, Deserialize)]
pub struct Haptics {
    pub device_id: u64,     // 触觉信息--设备id
    pub duration: Duration, // 触觉信息--持续时间
    pub frequency: f32,     // 触觉信息--频率
    pub amplitude: f32,     // 触觉信息--振幅
}

#[derive(Serialize, Deserialize, Debug)]
pub struct AudioDevicesList {
    pub output: Vec<String>,
    pub input: Vec<String>,
}

pub enum GpuVendor {
    // GPU厂商
    Nvidia,
    Amd,
    Other,
}

#[derive(Serialize, Deserialize, Clone)]
pub enum PathSegment {
    Name(String),
    Index(usize),
}

impl Debug for PathSegment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PathSegment::Name(name) => write!(f, "{}", name),
            PathSegment::Index(index) => write!(f, "[{}]", index),
        }
    }
}

impl From<&str> for PathSegment {
    fn from(value: &str) -> Self {
        PathSegment::Name(value.to_owned())
    }
}

impl From<String> for PathSegment {
    fn from(value: String) -> Self {
        PathSegment::Name(value)
    }
}

impl From<usize> for PathSegment {
    fn from(value: usize) -> Self {
        PathSegment::Index(value)
    }
}

// todo: support indices
pub fn parse_path(path: &str) -> Vec<PathSegment> {
    path.split('.').map(|s| s.into()).collect()
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum ClientListAction {
    // 客户端列表的可用操作
    AddIfMissing {
        trusted: bool,
        manual_ips: Vec<IpAddr>,
    },
    SetDisplayName(String),
    Trust,
    SetManualIps(Vec<IpAddr>),
    RemoveEntry,
    UpdateCurrentIp(Option<IpAddr>),
}

#[derive(Serialize, Deserialize, Default, Clone)]
pub struct ClientStatistics {
    // 客户端统计信息
    pub target_timestamp: Duration, // identifies the frame
    pub frame_interval: Duration,
    pub video_decode: Duration,
    pub video_decoder_queue: Duration,
    pub rendering: Duration,
    pub vsync_queue: Duration,
    pub total_pipeline_latency: Duration,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct PathValuePair {
    pub path: Vec<PathSegment>,
    pub value: serde_json::Value,
}

#[derive(Serialize, Deserialize, Debug)]
pub enum DashboardRequest {
    Ping,
    Log(LogEvent),
    GetSession,
    UpdateSession(Box<SessionDesc>),
    SetValues(Vec<PathValuePair>),
    UpdateClientList {
        hostname: String,
        action: ClientListAction,
    },
    GetAudioDevices,
    CaptureFrame,
    InsertIdr,
    StartRecording,
    StopRecording,
    RestartSteamvr,
    ShutdownSteamvr,
}

#[derive(Serialize, Deserialize, Debug)]
pub enum ServerResponse {
    AudioDevices(AudioDevicesList),
}
