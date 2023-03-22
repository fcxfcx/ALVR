use crate::FfiDynamicEncoderParams;
use alvr_common::SlidingWindowAverage;
use alvr_session::{BitrateConfig, BitrateMode};
use settings_schema::Switch;
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

const UPDATE_INTERVAL: Duration = Duration::from_secs(1);


//定义一个bitratemanager结构体
pub struct BitrateManager {
    config: BitrateConfig,    //新增：比特率配置
    max_history_size: usize,    //新增：历史最大值
    frame_interval_average: SlidingWindowAverage<Duration>,  //帧间隔平均值
    packet_sizes_bits_history: VecDeque<(Duration, usize)>,   //新增：历史包大小记录
    network_latency_average: SlidingWindowAverage<Duration>,  //网络延迟平均值
    bitrate_average: SlidingWindowAverage<u64>,      //新增：平均比特率
    decoder_latency_overstep_count: usize,          //新增：解码延迟计数器
    last_frame_instant: Instant,   //上一帧的时刻
    last_update_instant: Instant,  //上一次更新
    dynamic_max_bitrate: u64,   //新增：动态最大比特率
    update_needed: bool,    //更新需求
}

impl BitrateManager {
    //构造函数，创建一个实例
    pub fn new(config: BitrateConfig, max_history_size: usize) -> Self {
        Self {
            config,   
            max_history_size,
            frame_interval_average: SlidingWindowAverage::new(  //对滑动窗口实例化，并把参数：帧间隔初始值16ms和历史最大值给 frame_interval_average
                Duration::from_millis(16),
                max_history_size,
            ),
            packet_sizes_bits_history: VecDeque::new(),
            network_latency_average: SlidingWindowAverage::new(//对滑动窗口实例化，并把参数：网络延迟初始值5ms和历史最大值给 network_latency_average
                Duration::from_millis(5),
                max_history_size,
            ),
            bitrate_average: SlidingWindowAverage::new(30_000_000, max_history_size), //对滑动窗口实例化，并把参数：比特率初始值30和历史最大值给 bitrate_average
            decoder_latency_overstep_count: 0, 
            last_frame_instant: Instant::now(), //获取当前时间并把参数传给last_frame_instant
            last_update_instant: Instant::now(), //获取当前时间并把参数传给last_update_instant
            dynamic_max_bitrate: u64::MAX,  //获取动态比特率最大值
            update_needed: true,   //更新需求设置为true
        }
    }

    // Note: This is used to calculate the framerate/frame interval. The frame present is the most
    // accurate event for this use.

    //计算帧间隔平均值
    pub fn report_frame_resent(&mut self) {
        let now = Instant::now(); //获取当前时间

        let interval = now - self.last_frame_instant; //间隔=当前-上一帧时刻
        self.last_frame_instant = now; //计算完后把上一帧的时刻设置为当前时间

       
        // 如果帧间隔时间超过平均值=间隔/平均帧间隔
        //If the latest frame interval deviates too much from the mean,
        let interval_ratio =
            interval.as_secs_f32() / self.frame_interval_average.get_average().as_secs_f32();

        self.frame_interval_average.submit_sample(interval);
        //往存帧间隔的窗口传输样本值

        if (interval_ratio - 1.0).abs() > self.config.framerate_reset_threshold_multiplier {
            self.frame_interval_average =
                SlidingWindowAverage::new(interval, self.max_history_size);//计算帧间隔平均值
            self.update_needed = true; //更新
        }
    }

    //报告编码帧的大小，把每一帧的大小加入包大小历史记录中
    pub fn report_encoded_frame_size(&mut self, timestamp: Duration, size_bytes: usize) {
        self.packet_sizes_bits_history
            .push_back((timestamp, size_bytes * 8));
    }
    

    //解码延迟用于学习一个合适的最大比特率约束，以避免解码器失控。
    // decoder_latency is used to learn a suitable maximum bitrate bound to avoid decoder runaway
    // latency
    //报告帧延迟
    pub fn report_frame_latencies(
        &mut self,
        timestamp: Duration, //时间戳
        network_latency: Duration, //网路延迟
        decoder_latency: Duration, //解码延迟
    ) {
        //遍历包大小历史记录，更新码率平均值
        while let Some(&(timestamp_, size_bits)) = self.packet_sizes_bits_history.front() {
            if timestamp_ == timestamp {
                // 提交网络延迟和包大小的样本，更新码率平均值
                self.bitrate_average
                    .submit_sample((size_bits as f32 / network_latency.as_secs_f32()) as u64);

                   // 移除历史记录中的第一个元素
                self.packet_sizes_bits_history.pop_front();
                //// 退出循环

                break;
            } else {
                // 移除历史记录中的第一个元素
                self.packet_sizes_bits_history.pop_front();
            }
        }

        //如果解码延迟大于最大解码延迟
        if decoder_latency > Duration::from_millis(self.config.max_decoder_latency_ms) {
            //解码延迟超出次数+1
            self.decoder_latency_overstep_count += 1;
            
            // 如果解码器延迟超限计数器等于指定的帧数，则更新最大码率并标记需要更新
            if self.decoder_latency_overstep_count
                == self.config.decoder_latency_overstep_frames as usize
            {  //动态最大比特率=min（平均值，动态最大比特率）*解码延迟超限系数
                self.dynamic_max_bitrate =
                    (u64::min(self.bitrate_average.get_average(), self.dynamic_max_bitrate) as f32
                        * self.config.decoder_latency_overstep_multiplier)
                        as u64;
               
                self.update_needed = true;
                
                //重置解码延迟超限计数器
                self.decoder_latency_overstep_count = 0;
            }
        } else {
            // 如果解码器延迟没有超过最大延迟值，重置解码器延迟超限计数器
            self.decoder_latency_overstep_count = 0;
        }
    }

     //报告一些编码参数
    pub fn get_encoder_params(&mut self) -> FfiDynamicEncoderParams {
        //获取当前时间
        let now = Instant::now();
        //检查是否需要更新编码器参数
        if self.update_needed || now > self.last_update_instant + UPDATE_INTERVAL {
            //如果需要更新参数，记录当前时间作为最后更新时间
            self.last_update_instant = now;
        } else {
            //如果不需要更新参数，返回一个默认的 FfiDynamicEncoderParams
            return FfiDynamicEncoderParams {
                updated: false,
                bitrate_bps: 0,
                framerate: 0.0,
            };
        }

         //计算比特率
        let mut bitrate_bps = match &self.config.mode {
            //如果使用恒定比特率模式
            BitrateMode::ConstantMbps(bitrate_mbs) => *bitrate_mbs * 1_000_000,
             //如果使用自适应比特率模式
            BitrateMode::Adaptive {
                saturation_multiplier,
                max_bitrate_mbps,
                min_bitrate_mbps,
            } => {
                 //计算平均比特率并乘以饱和度因子
                let mut bitrate_bps =
                    (self.bitrate_average.get_average() as f32 * saturation_multiplier) as u64;
                 //如果指定了最大比特率，限制比特率不超过最大比特率
                if let Switch::Enabled(max) = max_bitrate_mbps {
                    bitrate_bps = u64::min(bitrate_bps, max * 1_000_000);
                }
                //如果指定了最小比特率，限制比特率不低于最小比特率
                if let Switch::Enabled(min) = min_bitrate_mbps {
                    bitrate_bps = u64::max(bitrate_bps, min * 1_000_000);
                }
                 //返回计算出的比特率
                bitrate_bps
            }
        };
        //如果指定了最大网络延迟，根据网络延迟和最大网络延迟计算比特率
        if let Switch::Enabled(max_ms) = &self.config.max_network_latency_ms {
            let multiplier =
                *max_ms as f32 / 1000.0 / self.network_latency_average.get_average().as_secs_f32();
            bitrate_bps = u64::min(bitrate_bps, (bitrate_bps as f32 * multiplier) as u64);
        }
        

        //限制比特率不超过动态最大比特率
        bitrate_bps = u64::min(bitrate_bps, self.dynamic_max_bitrate);
        

          //计算帧率
        let framerate = 1.0
            / self
                .frame_interval_average
                .get_average()
                .as_secs_f32()
                .min(1.0);
            
        //返回计算出的 FfiDynamicEncoderParams
        FfiDynamicEncoderParams {
            updated: true,
            bitrate_bps,
            framerate,
        }
    }
}
