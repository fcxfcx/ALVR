use alvr_common::{SlidingWindowAverage, HEAD_ID, LEFT_HAND_ID, RIGHT_HAND_ID};
use alvr_events::{EventType, GraphStatistics, Statistics};
use alvr_sockets::ClientStatistics;
use std::{
    collections::{HashMap, VecDeque},
    time::{Duration, Instant},
};

const FULL_REPORT_INTERVAL: Duration = Duration::from_millis(500);

//结构体：历史帧
pub struct HistoryFrame {
    target_timestamp: Duration,       //目标时间戳
    tracking_received: Instant,       //帧接收时间
    frame_present: Instant,           //帧呈现时间
    frame_composed: Instant,          //帧组成时间
    frame_encoded: Instant,           //帧编码时间
    total_pipeline_latency: Duration, //总流程延迟时间
}

impl Default for HistoryFrame {
    // `default` 方法用于创建一个默认值的 `HistoryFrame` 实例
    fn default() -> Self {
        let now = Instant::now(); //获取当前时间
        Self {
            target_timestamp: Duration::ZERO,       //目标时间戳，初始化为0
            tracking_received: now,                 //接受时间，初始化为当前时间
            frame_present: now,                     // 帧呈现时间，初始化为当前时间
            frame_composed: now,                    // 帧合成时间，初始化为当前时间
            frame_encoded: now,                     // 帧编码时间，初始化为当前时间
            total_pipeline_latency: Duration::ZERO, // 总延迟，初始化为 0
        }
    }
}

pub struct StatisticsManager {
    history_buffer: VecDeque<HistoryFrame>, // 历史帧缓冲区，用于记录历史帧的信息
    max_history_size: usize,                // 最大历史帧缓存大小
    last_full_report_instant: Instant,      // 上一次完整的报告时间
    last_frame_present_instant: Instant,    // 上一帧图像呈现时间
    last_frame_present_interval: Duration,  // 上一帧图像呈现间隔时长
    video_packets_total: usize,             // 视频数据包总数
    video_packets_partial_sum: usize,       //视频数据包部分求和
    video_bytes_total: usize,               //视频数据总字节数
    video_bytes_partial_sum: usize,         //视频数据部分字节求和
    packets_lost_total: usize,              //丢包总数
    packets_lost_partial_sum: usize,        //丢包部分求和
    battery_gauges: HashMap<u64, f32>,      //电池电量表，用于存储设备ID和对应的电量值
    steamvr_pipeline_latency: Duration,
    total_pipeline_latency_average: SlidingWindowAverage<Duration>,
}

impl StatisticsManager {
    // history size used to calculate average total pipeline latency
    pub fn new(
        max_history_size: usize,
        nominal_server_frame_interval: Duration,
        steamvr_pipeline_frames: f32,
    ) -> Self {
        Self {
            history_buffer: VecDeque::new(), // 历史帧缓冲区，用于记录历史帧的信息
            max_history_size,                // 最大历史帧缓存大小
            last_full_report_instant: Instant::now(), // 上一次完整的报告时间
            last_frame_present_instant: Instant::now(), // 上一帧图像呈现时间
            last_frame_present_interval: Duration::ZERO, // 上一帧图像呈现间隔时长
            video_packets_total: 0,          // 视频数据包总数
            video_packets_partial_sum: 0,    //视频数据包部分求和
            video_bytes_total: 0,            //视频数据总字节数
            video_bytes_partial_sum: 0,      //视频数据部分字节求和
            packets_lost_total: 0,           //丢包总数
            packets_lost_partial_sum: 0,     //丢包部分求和
            battery_gauges: HashMap::new(),  //电池电量表，用于存储设备ID和对应的电量值
            steamvr_pipeline_latency: Duration::from_secs_f32(
                steamvr_pipeline_frames * nominal_server_frame_interval.as_secs_f32(),
            ),
            total_pipeline_latency_average: SlidingWindowAverage::new(
                Duration::ZERO,
                max_history_size,
            ),
        }
    }

    pub fn report_tracking_received(&mut self, target_timestamp: Duration) {
        // 如果历史缓存中没有该时间戳的帧，则执行以下操作
        if !self
            .history_buffer
            .iter()
            .any(|frame| frame.target_timestamp == target_timestamp)
        {
            // 将带有目标时间戳和当前时间的帧添加到历史缓存的前面
            self.history_buffer.push_front(HistoryFrame {
                target_timestamp,
                tracking_received: Instant::now(),
                ..Default::default()
            });
        }
        // 如果历史缓存的长度超过最大历史缓存长度，则将最后一个元素弹出
        if self.history_buffer.len() > self.max_history_size {
            self.history_buffer.pop_back();
        }
    }

    pub fn report_frame_present(&mut self, target_timestamp: Duration, offset: Duration) {
        // 遍历历史缓冲区，查找目标时间戳对应的帧
        if let Some(frame) = self
            .history_buffer
            .iter_mut()
            .find(|frame| frame.target_timestamp == target_timestamp)
        {
            // 计算当前时间并减去时间偏移量，获得实际的当前时间
            let now = Instant::now() - offset;
            // 计算当前帧呈现时间与上一帧呈现时间的时间间隔
            self.last_frame_present_interval =
                now.saturating_duration_since(self.last_frame_present_instant);
            // 更新上一帧呈现时间为当前时间
            self.last_frame_present_instant = now;
            // 记录当前帧呈现时间
            frame.frame_present = now;
        }
    }

    pub fn report_frame_composed(&mut self, target_timestamp: Duration, offset: Duration) {
        // 使用迭代器方法iter_mut()遍历历史缓冲区（history_buffer）中的所有元素，通过find()查找符合条件的元素
        // 使用闭包指定查找条件，即frame的target_timestamp属性值等于传入的target_timestamp
        if let Some(frame) = self
            .history_buffer
            .iter_mut()
            .find(|frame| frame.target_timestamp == target_timestamp)
        {
            // 如果找到符合条件的元素，则将该元素的frame_composed属性值设置为当前时间减去偏移量
            frame.frame_composed = Instant::now() - offset;
        }
    }

    pub fn report_frame_encoded(&mut self, target_timestamp: Duration) {
        if let Some(frame) = self
            .history_buffer
            .iter_mut()
            .find(|frame| frame.target_timestamp == target_timestamp)
        {
            frame.frame_encoded = Instant::now();
        }
    }

    pub fn report_video_packet(&mut self, bytes_count: usize) {
        self.video_packets_total += 1;
        self.video_packets_partial_sum += 1;
        self.video_bytes_total += bytes_count;
        self.video_bytes_partial_sum += bytes_count;
    }

    pub fn report_packet_loss(&mut self) {
        self.packets_lost_total += 1;
        self.packets_lost_partial_sum += 1;
    }

    pub fn report_battery(&mut self, device_id: u64, gauge_value: f32) {
        *self.battery_gauges.entry(device_id).or_default() = gauge_value;
    }

    // Called every frame. Some statistics are reported once every frame
    // Returns network latency
    //定义一个公共函数，输入参数为可变的self引用和客户端统计信息client_stats，并返回一个时间间隔Duration
    pub fn report_statistics(&mut self, client_stats: ClientStatistics) -> Duration {
        //如果可变的history_buffer迭代器中的一个frame.target_timestamp等于client_stats.target_timestamp，
        //则将该frame的total_pipeline_latency属性更新为client_stats的total_pipeline_latency属性，
        if let Some(frame) = self
            .history_buffer
            .iter_mut()
            .find(|frame| frame.target_timestamp == client_stats.target_timestamp)
        {
            //并计算出网络延迟、客户端帧率和服务器帧率等统计信息
            frame.total_pipeline_latency = client_stats.total_pipeline_latency;
            //计算出游戏渲染延迟时间
            let game_time_latency = frame
                .frame_present
                .saturating_duration_since(frame.tracking_received);

            //计算出服务器合成延迟时间
            let server_compositor_latency = frame
                .frame_composed
                .saturating_duration_since(frame.frame_present);

            //计算出编码延迟时间
            let encoder_latency = frame
                .frame_encoded
                .saturating_duration_since(frame.frame_composed);

            // The network latency cannot be estiamed directly. It is what's left of the total
            // latency after subtracting all other latency intervals. In particular it contains the
            // transport latency of the tracking packet and the interval between the first video
            // packet is sent and the last video packet is received for a specific frame.
            // For safety, use saturating_sub to avoid a crash if for some reason the network
            // latency is miscalculated as negative.
            //网络延迟不能直接估算，它是总延迟减去所有其他延迟间隔的剩余部分。
            //特别地，它包含跟踪包的传输延迟和针对特定帧发送第一个视频包和接收最后一个视频包之间的时间间隔。
            //为了安全起见，使用saturating_sub避免如果网络延迟被错误地计算为负数而导致崩溃。
            let network_latency = frame.total_pipeline_latency.saturating_sub(
                game_time_latency
                    + server_compositor_latency
                    + encoder_latency
                    + client_stats.video_decode
                    + client_stats.video_decoder_queue
                    + client_stats.rendering
                    + client_stats.vsync_queue,
            );
            //计算出客户端帧率和服务器帧率
            let client_fps = 1.0
                / client_stats
                    .frame_interval
                    .max(Duration::from_millis(1))
                    .as_secs_f32();
            let server_fps = 1.0
                / self
                    .last_frame_present_interval
                    .max(Duration::from_millis(1))
                    .as_secs_f32();
            //如果距离上一次完整报告时间间隔已经超过FULL_REPORT_INTERVAL，则发送EventType::Statistics类型的事件，
            //包含视频包数、每秒视频包数、视频总字节数、每秒视频比特率、总延迟时间、网络延迟时间、编码延迟时间、
            //解码延迟时间、视频包丢失总数、每秒视频包丢失数、客户端帧率、服务器帧率以及头盔
            if self.last_full_report_instant + FULL_REPORT_INTERVAL < Instant::now() {
                self.last_full_report_instant += FULL_REPORT_INTERVAL;

                let interval_secs = FULL_REPORT_INTERVAL.as_secs_f32();

                alvr_events::send_event(EventType::Statistics(Statistics {
                    video_packets_total: self.video_packets_total,
                    video_packets_per_sec: (self.video_packets_partial_sum as f32 / interval_secs)
                        as _,
                    video_mbytes_total: (self.video_bytes_total as f32 / 1e6) as usize,
                    video_mbits_per_sec: self.video_bytes_partial_sum as f32 / interval_secs * 8.
                        / 1e6,
                    total_latency_ms: client_stats.total_pipeline_latency.as_secs_f32() * 1000.,
                    network_latency_ms: network_latency.as_secs_f32() * 1000.,
                    encode_latency_ms: encoder_latency.as_secs_f32() * 1000.,
                    decode_latency_ms: client_stats.video_decode.as_secs_f32() * 1000.,
                    packets_lost_total: self.packets_lost_total,
                    packets_lost_per_sec: (self.packets_lost_partial_sum as f32 / interval_secs)
                        as _,
                    client_fps: client_fps as _,
                    server_fps: server_fps as _,
                    battery_hmd: (self
                        .battery_gauges
                        .get(&HEAD_ID)
                        .cloned()
                        .unwrap_or_default()
                        * 100.) as _,
                    battery_left: (self
                        .battery_gauges
                        .get(&LEFT_HAND_ID)
                        .cloned()
                        .unwrap_or_default()
                        * 100.) as _,
                    battery_right: (self
                        .battery_gauges
                        .get(&RIGHT_HAND_ID)
                        .cloned()
                        .unwrap_or_default()
                        * 100.) as _,
                }));

                self.video_packets_partial_sum = 0;
                self.video_bytes_partial_sum = 0;
                self.packets_lost_partial_sum = 0;
            }

            // todo: use target timestamp in nanoseconds. the dashboard needs to use the first
            // timestamp as the graph time origin.
            alvr_events::send_event(EventType::GraphStatistics(GraphStatistics {
                total_pipeline_latency_s: client_stats.total_pipeline_latency.as_secs_f32(),
                game_time_s: game_time_latency.as_secs_f32(),
                server_compositor_s: server_compositor_latency.as_secs_f32(),
                encoder_s: encoder_latency.as_secs_f32(),
                network_s: network_latency.as_secs_f32(),
                decoder_s: client_stats.video_decode.as_secs_f32(),
                decoder_queue_s: client_stats.video_decoder_queue.as_secs_f32(),
                client_compositor_s: client_stats.rendering.as_secs_f32(),
                vsync_queue_s: client_stats.vsync_queue.as_secs_f32(),
                client_fps,
                server_fps,
            }));

            network_latency
        } else {
            Duration::ZERO
        }
    }

    pub fn tracker_pose_time_offset(&self) -> Duration {
        // This is the opposite of the client's StatisticsManager::tracker_prediction_offset().
        self.steamvr_pipeline_latency
            .saturating_sub(self.total_pipeline_latency_average.get_average())
    }
}
