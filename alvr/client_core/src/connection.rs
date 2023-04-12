#![allow(clippy::if_same_then_else)]

use crate::{
    decoder::{self, DECODER_INIT_CONFIG},
    platform,
    sockets::AnnouncerSocket,
    statistics::StatisticsManager,
    storage::Config,
    ClientCoreEvent, CONTROL_CHANNEL_SENDER, DISCONNECT_NOTIFIER, EVENT_QUEUE, IS_ALIVE,
    IS_RESUMED, IS_STREAMING, STATISTICS_MANAGER, STATISTICS_SENDER, TRACKING_SENDER,
};
use alvr_audio::AudioDevice;
use alvr_common::{glam::UVec2, prelude::*, ALVR_VERSION, HEAD_ID};
use alvr_session::{settings_schema::Switch, SessionDesc};
use alvr_sockets::{
    spawn_cancelable, BatteryPacket, ClientConnectionResult, ClientControlPacket, Haptics,
    PeerType, ProtoControlSocket, ReceiverBuffer, ServerControlPacket, StreamConfigPacket,
    StreamSocketBuilder, VideoStreamingCapabilities, AUDIO, HAPTICS, STATISTICS, TRACKING, VIDEO,
};
use futures::future::BoxFuture;
use serde_json as json;
use std::{
    future,
    net::IpAddr,
    sync::Arc,
    thread,
    time::{Duration, Instant},
};
use tokio::{
    runtime::Runtime,
    sync::{mpsc as tmpsc, Mutex},
    time,
};

#[cfg(target_os = "android")]
use crate::audio;
#[cfg(not(target_os = "android"))]
use alvr_audio as audio;

const INITIAL_MESSAGE: &str = concat!(
    "Searching for streamer...\n",
    "Open ALVR on your PC then click \"Trust\"\n",
    "next to the client entry",
);
const NETWORK_UNREACHABLE_MESSAGE: &str = "Cannot connect to the internet";
// const INCOMPATIBLE_VERSIONS_MESSAGE: &str = concat!(
//     "Streamer and client have\n",
//     "incompatible types.\n",
//     "Please update either the app\n",
//     "on the PC or on the headset",
// );
const STREAM_STARTING_MESSAGE: &str = "The stream will begin soon\nPlease wait...";
const SERVER_RESTART_MESSAGE: &str = "The streamer is restarting\nPlease wait...";
const SERVER_DISCONNECTED_MESSAGE: &str = "The streamer has disconnected.";

const DISCOVERY_RETRY_PAUSE: Duration = Duration::from_millis(500);
const RETRY_CONNECT_MIN_INTERVAL: Duration = Duration::from_secs(1);
const NETWORK_KEEPALIVE_INTERVAL: Duration = Duration::from_secs(1);
const CONNECTION_RETRY_INTERVAL: Duration = Duration::from_secs(1);
const BATTERY_POLL_INTERVAL: Duration = Duration::from_secs(60);

// 记录hud信息
fn set_hud_message(message: &str) {
    let message = format!(
        "ALVR v{}\nhostname: {}\nIP: {}\n\n{message}",
        *ALVR_VERSION,
        Config::load().hostname,
        platform::local_ip(),
    );

    EVENT_QUEUE
        .lock()
        .push_back(ClientCoreEvent::UpdateHudMessage(message));
}

pub fn connection_lifecycle_loop(
    recommended_view_resolution: UVec2,
    supported_refresh_rates: Vec<f32>,
) -> IntResult {
    // 设置初始化hud信息
    set_hud_message(INITIAL_MESSAGE);

    let decoder_guard = Arc::new(Mutex::new(()));

    loop {
        // 如果IS_ALIVE.value()为false则中断
        check_interrupt!(IS_ALIVE.value());

        if IS_RESUMED.value() {
            if let Err(e) = connection_pipeline(
                recommended_view_resolution,
                supported_refresh_rates.clone(),
                Arc::clone(&decoder_guard),
            ) {
                match e {
                    InterruptibleError::Interrupted => return Ok(()),
                    InterruptibleError::Other(_) => {
                        let message =
                            format!("Connection error:\n{e}\nCheck the PC for more details");
                        error!("{message}");
                        set_hud_message(&message);
                    }
                }
            }
        }

        thread::sleep(CONNECTION_RETRY_INTERVAL);
    }
}

fn connection_pipeline(
    recommended_view_resolution: UVec2,
    supported_refresh_rates: Vec<f32>,
    decoder_guard: Arc<Mutex<()>>,
) -> IntResult {
    let runtime = Runtime::new().map_err(to_int_e!())?;

    // 建立TCP连接后，得到流以及对端ip，对端即server端
    let (mut proto_control_socket, server_ip) = {
        let config = Config::load();
        // 广播socket，用UDP
        let announcer_socket = AnnouncerSocket::new(&config.hostname).map_err(to_int_e!())?;
        // 监听socket，用TCP
        let listener_socket = runtime
            .block_on(alvr_sockets::get_server_listener())
            .map_err(to_int_e!())?;

        loop {
            check_interrupt!(IS_ALIVE.value());

            // UDP广播信息
            if let Err(e) = announcer_socket.broadcast() {
                warn!("Broadcast error: {e}");

                set_hud_message(NETWORK_UNREACHABLE_MESSAGE);

                thread::sleep(RETRY_CONNECT_MIN_INTERVAL);

                set_hud_message(INITIAL_MESSAGE);

                return Ok(());
            }

            // 建立TCP连接后，得到流以及对端ip
            let maybe_pair = runtime.block_on(async {
                tokio::select! {
                    maybe_pair = ProtoControlSocket::connect_to(PeerType::Server(&listener_socket)) => {
                        maybe_pair.map_err(to_int_e!())
                    },
                    _ = time::sleep(DISCOVERY_RETRY_PAUSE) => Err(InterruptibleError::Interrupted)
                }
            });

            if let Ok(pair) = maybe_pair {
                break pair;
            }
        }
    };

    // 麦克风采样率
    let microphone_sample_rate = AudioDevice::new_input(None)
        .unwrap()
        .input_sample_rate()
        .unwrap();

    // 利用已建立的TCP连接发送一些信息，比如展示名称、服务端ip、默认分辨率、支持的刷新率、麦克风采样率等等
    runtime
        .block_on(
            proto_control_socket.send(&ClientConnectionResult::ConnectionAccepted {
                display_name: platform::device_model(),
                server_ip,
                streaming_capabilities: Some(VideoStreamingCapabilities {
                    default_view_resolution: recommended_view_resolution,
                    supported_refresh_rates,
                    microphone_sample_rate,
                }),
            }),
        )
        .map_err(to_int_e!())?;

    // 阻塞读得到系统流配置包
    let config_packet = runtime
        .block_on(proto_control_socket.recv::<StreamConfigPacket>())
        .map_err(to_int_e!())?;

    runtime
        .block_on(stream_pipeline(
            proto_control_socket,
            config_packet,
            server_ip,
            decoder_guard,
        ))
        .map_err(to_int_e!())
}

async fn stream_pipeline(
    proto_socket: ProtoControlSocket,
    stream_config: StreamConfigPacket,
    server_ip: IpAddr,
    decoder_guard: Arc<Mutex<()>>,
) -> StrResult {
    let (control_sender, mut control_receiver) = proto_socket.split();
    let control_sender = Arc::new(Mutex::new(control_sender));

    // 阻塞读
    match control_receiver.recv().await {
        // 开始流
        Ok(ServerControlPacket::StartStream) => {
            info!("Stream starting");
            set_hud_message(STREAM_STARTING_MESSAGE);
        }
        // server重启
        Ok(ServerControlPacket::Restarting) => {
            info!("Server restarting");
            set_hud_message(SERVER_RESTART_MESSAGE);
            return Ok(());
        }
        // server断开
        Err(e) => {
            info!("Server disconnected. Cause: {e}");
            set_hud_message(SERVER_DISCONNECTED_MESSAGE);
            return Ok(());
        }
        // 未知包
        _ => {
            info!("Unexpected packet");
            set_hud_message("Unexpected packet");
            return Ok(());
        }
    }

    // settings对象里保存了之前server发过来的流配置信息
    let settings = {
        let mut session_desc = SessionDesc::default();
        session_desc
            .merge_from_json(&json::from_str(&stream_config.session_desc).map_err(err!())?)?;
        session_desc.to_settings()
    };

    *STATISTICS_MANAGER.lock() = Some(StatisticsManager::new(
        settings.connection.statistics_history_size as _,
        Duration::from_secs_f32(1.0 / stream_config.fps),
        if let Switch::Enabled(config) = settings.headset.controllers {
            config.steamvr_pipeline_frames
        } else {
            0.0
        },
    ));

    // StreamSocketBuilder对象里面包含流端口号、协议是TCP/UDP、发送接收的buf字节数
    let stream_socket_builder = StreamSocketBuilder::listen_for_server(
        settings.connection.stream_port,
        settings.connection.stream_protocol,
        settings.connection.client_send_buffer_bytes,
        settings.connection.client_recv_buffer_bytes,
    )
    .await?;

    // 通过TCP控制连接发送StreamReady消息
    if let Err(e) = control_sender
        .lock()
        .await
        .send(&ClientControlPacket::StreamReady)
        .await
    {
        info!("Server disconnected. Cause: {e}");
        set_hud_message(SERVER_DISCONNECTED_MESSAGE);
        return Ok(());
    }

    // 主动和server建立流连接，返回stream_socket
    let stream_socket = tokio::select! {
        res = stream_socket_builder.accept_from_server(
            server_ip,
            settings.connection.stream_port,
            settings.connection.packet_size as _
        ) => res?,
        _ = time::sleep(Duration::from_secs(5)) => {
            return fmt_e!("Timeout while setting up streams");
        }
    };
    let stream_socket = Arc::new(stream_socket);

    info!("Connected to server");

    // create this before initializing the stream on cpp side
    // control_channel_sender和control_channel_receiver用来在异步任务间通信
    let (control_channel_sender, mut control_channel_receiver) = tmpsc::unbounded_channel();
    *CONTROL_CHANNEL_SENDER.lock() = Some(control_channel_sender);

    // 初始化编码器配置
    {
        let config = &mut *DECODER_INIT_CONFIG.lock();

        config.max_buffering_frames = settings.video.max_buffering_frames;
        config.buffering_history_weight = settings.video.buffering_history_weight;
        config.options = settings
            .video
            .advanced_codec_options
            .mediacodec_extra_options;
    }

    // 循环阻塞读data_receiver，收到hmd的tracking追踪信息后直接通过socket_sender转发给server
    let tracking_send_loop = {
        // 通过stream_socket获得socket_sender对象
        let mut socket_sender = stream_socket.request_stream(TRACKING).await?;
        async move {
            let (data_sender, mut data_receiver) = tmpsc::unbounded_channel();
            *TRACKING_SENDER.lock() = Some(data_sender);

            while let Some(tracking) = data_receiver.recv().await {
                socket_sender.send(&tracking, vec![]).await.ok();

                // Note: this is not the best place to report the acquired input. Instead it should
                // be done as soon as possible (or even just before polling the input). Instead this
                // is reported late to partially compensate for lack of network latency measurement,
                // so the server can just use total_pipeline_latency as the postTimeoffset.
                // This hack will be removed once poseTimeOffset can be calculated more accurately.
                if let Some(stats) = &mut *STATISTICS_MANAGER.lock() {
                    stats.report_input_acquired(tracking.target_timestamp);
                }
            }

            Ok(())
        }
    };

    // 循环阻塞读data_receiver，收到hmd的Statistics统计信息后直接通过socket_sender转发给server
    let statistics_send_loop = {
        let mut socket_sender = stream_socket.request_stream(STATISTICS).await?;
        async move {
            let (data_sender, mut data_receiver) = tmpsc::unbounded_channel();
            *STATISTICS_SENDER.lock() = Some(data_sender);

            while let Some(stats) = data_receiver.recv().await {
                socket_sender.send(&stats, vec![]).await.ok();
            }

            Ok(())
        }
    };

    // 流开始事件保存了一些信息
    let streaming_start_event = ClientCoreEvent::StreamingStarted {
        view_resolution: stream_config.view_resolution,
        fps: stream_config.fps,
        foveated_rendering: settings.video.foveated_rendering.into_option(),
        oculus_foveation_level: settings.video.oculus_foveation_level,
        dynamic_oculus_foveation: settings.video.dynamic_oculus_foveation,
    };

    IS_STREAMING.set(true);

    // 接收视频流的loop
    let video_receive_loop = {
        let mut receiver = stream_socket.subscribe_to_stream::<Duration>(VIDEO).await?;
        let disconnection_critera = settings.connection.disconnection_criteria;
        async move {
            let _decoder_guard = decoder_guard.lock().await;

            // close stream on Drop (manual disconnection or execution canceling)
            struct StreamCloseGuard;

            impl Drop for StreamCloseGuard {
                fn drop(&mut self) {
                    EVENT_QUEUE
                        .lock()
                        .push_back(ClientCoreEvent::StreamingStopped);

                    IS_STREAMING.set(false);

                    #[cfg(target_os = "android")]
                    {
                        *crate::decoder::DECODER_ENQUEUER.lock() = None;
                        *crate::decoder::DECODER_DEQUEUER.lock() = None;
                    }
                }
            }

            let _stream_guard = StreamCloseGuard;

            EVENT_QUEUE.lock().push_back(streaming_start_event);

            // 接收的缓冲区
            let mut receiver_buffer = ReceiverBuffer::new();
            // 超时断连计时器
            let mut disconnection_timer_begin = None;
            // 接收信息的循环
            loop {
                // 从receiver缓冲区中获取信息
                receiver.recv_buffer(&mut receiver_buffer).await?;
                let (timestamp, nal) = receiver_buffer.get()?;

                if !IS_RESUMED.value() {
                    break Ok(());
                }

                if let Some(stats) = &mut *STATISTICS_MANAGER.lock() {
                    stats.report_video_packet_received(timestamp);
                }

                // 把数据压到解码队列的队尾
                decoder::push_nal(timestamp, nal);

                // 丢包监测
                if receiver_buffer.had_packet_loss() {
                    if let Some(sender) = &*CONTROL_CHANNEL_SENDER.lock() {
                        sender.send(ClientControlPacket::VideoErrorReport).ok();
                    }
                }

                // 貌似在做一些QOE方面的检测，涉及到时延阈值与超时断连
                // todo 但是Switch::Enabled()的用处是？
                if let Switch::Enabled(criteria) = &disconnection_critera {
                    if let Some(stats) = &*STATISTICS_MANAGER.lock() {
                        // 比较预测时延与时延阈值，如果小于就不必继续判断断连超时
                        if stats.average_total_pipeline_latency()
                            < Duration::from_millis(criteria.latency_threshold_ms)
                        {
                            disconnection_timer_begin = None;
                        } else {
                            // 要拿到计时开始的时间
                            let begin = disconnection_timer_begin.unwrap_or_else(Instant::now);

                            // 判断是否超时
                            if Instant::now()
                                > begin + Duration::from_secs(criteria.sustain_duration_s)
                            {
                                DISCONNECT_NOTIFIER.notify_one();
                            }

                            disconnection_timer_begin = Some(begin);
                        }
                    }
                }
            }
        }
    };

    // 接收触觉流的loop
    let haptics_receive_loop = {
        let mut receiver = stream_socket
            .subscribe_to_stream::<Haptics>(HAPTICS)
            .await?;
        async move {
            loop {
                let haptics = receiver.recv_header_only().await?;

                EVENT_QUEUE.lock().push_back(ClientCoreEvent::Haptics {
                    device_id: haptics.device_id,
                    duration: haptics.duration,
                    frequency: haptics.frequency,
                    amplitude: haptics.amplitude,
                });
            }
        }
    };

    // 游戏声音loop
    let game_audio_loop: BoxFuture<_> = if let Switch::Enabled(config) = settings.audio.game_audio {
        let device = AudioDevice::new_output(None, None).map_err(err!())?;

        let game_audio_receiver = stream_socket.subscribe_to_stream(AUDIO).await?;
        Box::pin(audio::play_audio_loop(
            device,
            2,
            stream_config.game_audio_sample_rate,
            config.buffering,
            game_audio_receiver,
        ))
    } else {
        Box::pin(future::pending())
    };

    // 麦克风loop
    let microphone_loop: BoxFuture<_> = if matches!(settings.audio.microphone, Switch::Enabled(_)) {
        let device = AudioDevice::new_input(None).map_err(err!())?;

        let microphone_sender = stream_socket.request_stream(AUDIO).await?;
        Box::pin(audio::record_audio_loop(
            device,
            1,
            false,
            microphone_sender,
        ))
    } else {
        Box::pin(future::pending())
    };

    // 开启一个线程用来做事件轮询
    // Poll for events that need a constant thread (mainly for the JNI env)
    thread::spawn(|| {
        #[cfg(target_os = "android")]
        let vm = platform::vm();
        #[cfg(target_os = "android")]
        let _env = vm.attach_current_thread();

        let mut previous_hmd_battery_status = (0.0, false);
        let mut battery_poll_deadline = Instant::now();

        while IS_STREAMING.value() {
            // 每60s执行一次，更新hmd的电池状态
            if battery_poll_deadline < Instant::now() {
                let new_hmd_battery_status = platform::battery_status();

                if new_hmd_battery_status != previous_hmd_battery_status {
                    if let Some(sender) = &*CONTROL_CHANNEL_SENDER.lock() {
                        sender
                            .send(ClientControlPacket::Battery(BatteryPacket {
                                device_id: *HEAD_ID,
                                gauge_value: new_hmd_battery_status.0,
                                is_plugged: new_hmd_battery_status.1,
                            }))
                            .ok();

                        previous_hmd_battery_status = new_hmd_battery_status;
                    }
                }

                battery_poll_deadline += BATTERY_POLL_INTERVAL;
            }

            thread::sleep(Duration::from_secs(1));
        }
    });

    // 每隔1s通过负责控制的TCP连接发送一条KeepAlive消息，如果Server断开则loop退出
    let keepalive_sender_loop = {
        let control_sender = Arc::clone(&control_sender);
        async move {
            loop {
                let res = control_sender
                    .lock()
                    .await
                    .send(&ClientControlPacket::KeepAlive)
                    .await;
                if let Err(e) = res {
                    info!("Server disconnected. Cause: {e}");
                    set_hud_message(SERVER_DISCONNECTED_MESSAGE);
                    break Ok(());
                }

                time::sleep(NETWORK_KEEPALIVE_INTERVAL).await;
            }
        }
    };

    // 循环阻塞读control_channel_receiver的packet并通过负责控制的TCP连接发给server
    let control_send_loop = async move {
        while let Some(packet) = control_channel_receiver.recv().await {
            control_sender.lock().await.send(&packet).await.ok();
        }

        Ok(())
    };

    // 循环阻塞读server发过来的消息并做处理
    let control_receive_loop = async move {
        loop {
            match control_receiver.recv().await {
                Ok(ServerControlPacket::InitializeDecoder(config)) => {
                    decoder::create_decoder(config);
                }
                Ok(ServerControlPacket::Restarting) => {
                    info!("{SERVER_RESTART_MESSAGE}");
                    set_hud_message(SERVER_RESTART_MESSAGE);
                    break Ok(());
                }
                Ok(_) => (),
                Err(e) => {
                    info!("{SERVER_DISCONNECTED_MESSAGE} Cause: {e}");
                    set_hud_message(SERVER_DISCONNECTED_MESSAGE);
                    break Ok(());
                }
            }
        }
    };

    let receive_loop = async move { stream_socket.receive_loop().await };

    // Run many tasks concurrently. Threading is managed by the runtime, for best performance.
    // 所有loop的future异步执行，直到被取消
    tokio::select! {
        res = spawn_cancelable(receive_loop) => {
            if let Err(e) = res {
                info!("Server disconnected. Cause: {e}");
            }
            set_hud_message(
                SERVER_DISCONNECTED_MESSAGE
            );

            Ok(())
        },
        res = spawn_cancelable(game_audio_loop) => res,
        res = spawn_cancelable(microphone_loop) => res,
        res = spawn_cancelable(tracking_send_loop) => res,
        res = spawn_cancelable(statistics_send_loop) => res,
        res = spawn_cancelable(video_receive_loop) => res,
        res = spawn_cancelable(haptics_receive_loop) => res,
        res = spawn_cancelable(control_send_loop) => res,

        // keep these loops on the current task
        res = keepalive_sender_loop => res,
        res = control_receive_loop => res,

        _ = DISCONNECT_NOTIFIER.notified() => Ok(()),
    }
}
