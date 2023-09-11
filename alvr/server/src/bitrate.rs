use crate::FfiDynamicEncoderParams;
use alvr_common::SlidingWindowAverage;
use alvr_events::NominalBitrateStats;
use alvr_session::{
    settings_schema::Switch, BitrateAdaptiveFramerateConfig, BitrateConfig, BitrateMode,
};
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

const UPDATE_INTERVAL: Duration = Duration::from_secs(1);


pub struct BitrateManager {
    nominal_frame_interval: Duration,                        //新增：帧率
    frame_interval_average: SlidingWindowAverage<Duration>,
    // note: why packet_sizes_bits_history is a queue and not a sliding average? Because some
    // network samples will be dropped but not any packet size sample
    packet_sizes_bits_history: VecDeque<(Duration, usize)>,  //新增：历史包大小记录
    encoder_latency_average: SlidingWindowAverage<Duration>,
    network_latency_average: SlidingWindowAverage<Duration>, //网络延迟平均值
    bitrate_average: SlidingWindowAverage<f32>,              //新增：平均比特率
    decoder_latency_overstep_count: usize,                   //新增：解码延迟计数器
    last_frame_instant: Instant,                             //上一帧的时刻
    last_update_instant: Instant,                            //上一次更新
    dynamic_max_bitrate: f32,                                //新增：动态最大比特率
    update_needed: bool,                                     //更新需求
    previous_config: Option<BitrateConfig>,
    bitrate_last_interval : VecDeque<f32>,       //上一个统计间隔内的带宽值
    data_last_interval : VecDeque<f32>           //上一个统计间隔内的数据量
}

impl BitrateManager {
    pub fn new(max_history_size: usize, initial_framerate: f32) -> Self {
        Self {
            nominal_frame_interval: Duration::from_secs_f32(1. / initial_framerate),
            frame_interval_average: SlidingWindowAverage::new(
                Duration::from_millis(16),
                max_history_size,
            ),
            packet_sizes_bits_history: VecDeque::new(),
            encoder_latency_average: SlidingWindowAverage::new(
                Duration::from_millis(5),
                max_history_size,
            ),
            network_latency_average: SlidingWindowAverage::new(
                Duration::from_millis(5),
                max_history_size,
            ),
            bitrate_average: SlidingWindowAverage::new(30_000_000.0, max_history_size),
            decoder_latency_overstep_count: 0,
            last_frame_instant: Instant::now(),
            last_update_instant: Instant::now(),
            dynamic_max_bitrate: f32::MAX,
            previous_config: None,
            update_needed: true,
            bitrate_last_interval: VecDeque::new(),
            data_last_interval: VecDeque::new()
        }
    }

    // Note: This is used to calculate the framerate/frame interval. The frame present is the most
    // accurate event for this use.

    //计算帧间隔平均值
    pub fn report_frame_present(&mut self, config: &Switch<BitrateAdaptiveFramerateConfig>) {

        let now = Instant::now();

        let interval = now - self.last_frame_instant; //间隔=当前-上一帧时刻
        self.last_frame_instant = now; //计算完后把上一帧的时刻设置为当前时间

        if let Some(config) = config.as_option() {
            let interval_ratio =
                interval.as_secs_f32() / self.frame_interval_average.get_average().as_secs_f32();

            //往存帧间隔的窗口传输样本值
            self.frame_interval_average.submit_sample(interval);

            if interval_ratio > config.framerate_reset_threshold_multiplier
                || interval_ratio < 1.0 / config.framerate_reset_threshold_multiplier
            {
                // Clear most of the samples, keep some for stability
                self.frame_interval_average.retain(5);
                self.update_needed = true;
            }
        }
    }

    pub fn report_frame_encoded(
        &mut self,
        timestamp: Duration,
        encoder_latency: Duration,
        size_bytes: usize,
    ) {
        self.encoder_latency_average.submit_sample(encoder_latency);

        self.packet_sizes_bits_history
            .push_back((timestamp, size_bytes * 8));
    }

    // decoder_latency is used to learn a suitable maximum bitrate bound to avoid decoder runaway
    // latency
    pub fn report_frame_latencies(
        &mut self,
        config: &BitrateMode,
        timestamp: Duration,
        network_latency: Duration,
        decoder_latency: Duration,
    ) {
        if network_latency.is_zero() {
            return;
        }

        self.network_latency_average.submit_sample(network_latency);

        while let Some(&(timestamp_, size_bits)) = self.packet_sizes_bits_history.front() {
            if timestamp_ == timestamp {
                self.bitrate_average
                    .submit_sample(size_bits as f32 / network_latency.as_secs_f32());
                self.bitrate_last_interval.push_back(size_bits as f32 / network_latency.as_secs_f32());
                self.data_last_interval.push_back(size_bits as f32);
                self.packet_sizes_bits_history.pop_front();

                break;
            } else {
                self.packet_sizes_bits_history.pop_front();
            }
        }

        if let BitrateMode::Adaptive {
            decoder_latency_limiter: Switch::Enabled(config),
            ..
        } = &config
        {
            if decoder_latency > Duration::from_millis(config.max_decoder_latency_ms) {
                self.decoder_latency_overstep_count += 1;
                // 如果解码器延迟超限计数器等于指定的帧数，则更新最大码率并标记需要更新
                // 如果解码器延迟超限计数器等于指定的帧数，则更新最大码率并标记需要更新
                if self.decoder_latency_overstep_count == config.latency_overstep_frames {
                    self.dynamic_max_bitrate =
                        f32::min(self.bitrate_average.get_average(), self.dynamic_max_bitrate)
                            * config.latency_overstep_multiplier;
                    self.update_needed = true;

                    self.decoder_latency_overstep_count = 0;
                }
            } else {
                self.decoder_latency_overstep_count = 0;
            }
        }
    }

    pub fn get_encoder_params(
        &mut self,
        config: &BitrateConfig,
    ) -> (FfiDynamicEncoderParams, Option<NominalBitrateStats>) {
        let now = Instant::now();

        if self
            .previous_config
            .as_ref()
            .map(|prev| config != prev)
            .unwrap_or(true)
        {
            self.previous_config = Some(config.clone());
            // Continue method. Always update bitrate in this case
        } else if !self.update_needed
            && (now < self.last_update_instant + UPDATE_INTERVAL
                || matches!(config.mode, BitrateMode::ConstantMbps(_)))
        {
            return (
                FfiDynamicEncoderParams {
                    updated: 0,
                    bitrate_bps: 0,
                    framerate: 0.0,
                },
                None,
            );
        }

        
        self.last_update_instant = now;
        self.update_needed = false;
        let mut stats = NominalBitrateStats::default();

        //计算比特率
        let bitrate_bps = match &config.mode {
            //如果使用恒定比特率模式
            BitrateMode::ConstantMbps(bitrate_mbps) => *bitrate_mbps as f32 * 1e6,
            BitrateMode::Adaptive {
                saturation_multiplier,
                max_bitrate_mbps,
                min_bitrate_mbps,
                max_network_latency_ms,
                encoder_latency_limiter,
                ..
            } => {
                let initial_bitrate_average_bps = self.bitrate_average.get_average();

                let mut bitrate_bps = initial_bitrate_average_bps * saturation_multiplier;
                stats.scaled_calculated_bps = Some(bitrate_bps);

                bitrate_bps = f32::min(bitrate_bps, self.dynamic_max_bitrate);
                stats.decoder_latency_limiter_bps = Some(self.dynamic_max_bitrate);

                //如果指定了最大网络延迟，根据网络延迟和最大网络延迟计算比特率
                if let Switch::Enabled(max_ms) = max_network_latency_ms {
                    let max = initial_bitrate_average_bps * (*max_ms as f32 / 1000.0)
                        / self.network_latency_average.get_average().as_secs_f32();
                    bitrate_bps = f32::min(bitrate_bps, max);

                    stats.network_latency_limiter_bps = Some(max);
                }

                if let Switch::Enabled(config) = encoder_latency_limiter {
                    let saturation = self.encoder_latency_average.get_average().as_secs_f32()
                        / self.nominal_frame_interval.as_secs_f32();
                    let max =
                        initial_bitrate_average_bps * config.max_saturation_multiplier / saturation;
                    stats.encoder_latency_limiter_bps = Some(max);

                    if saturation > config.max_saturation_multiplier {
                        // Note: this assumes linear relationship between bitrate and encoder
                        // latency but this may not be the case
                        bitrate_bps = f32::min(bitrate_bps, max);
                    }
                }

                if let Switch::Enabled(max) = max_bitrate_mbps {
                    let max = *max as f32 * 1e6;
                    bitrate_bps = f32::min(bitrate_bps, max);

                    stats.manual_max_bps = Some(max);
                }
                if let Switch::Enabled(min) = min_bitrate_mbps {
                    let min = *min as f32 * 1e6;
                    bitrate_bps = f32::max(bitrate_bps, min);

                    stats.manual_min_bps = Some(min);
                }

                //返回计算出的比特率
                bitrate_bps
            }
        };


        stats.requested_bps = bitrate_bps;

        let frame_interval = if config.adapt_to_framerate.enabled() {
            self.frame_interval_average.get_average()
        } else {
            self.nominal_frame_interval
        };

        (
            FfiDynamicEncoderParams {
                updated: 1,
                bitrate_bps: bitrate_bps as u64,
                framerate: 1.0 / frame_interval.as_secs_f32().min(1.0),
            },
            Some(stats),
        )
    }

    pub fn get_bitrate_last_interval(& self)-> f32{
        return self.bitrate_last_interval.iter().sum::<f32>() / self.bitrate_last_interval.len() as f32
    }

    pub fn get_data_last_interval(&self) -> f32{
        return self.data_last_interval.iter().sum::<f32>()
    }
    pub fn clear_last_interval(&mut self) {
        self.bitrate_last_interval.clear();
        self.data_last_interval.clear()
    }

}
