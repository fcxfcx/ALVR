mod gui_handler;

use alvr_events::Event;
use futures_util::StreamExt;
use std::time::Duration;
use tokio::sync::{
    broadcast,
    mpsc::{self, error::TryRecvError},
};
use tokio_tungstenite::{connect_async, tungstenite};

use crate::{GuiMsg, WorkerMsg};

const BASE_URL: &str = "http://localhost:8082";
const BASE_WS_URL: &str = "ws://localhost:8082";

pub fn http_thread(
    tx1: std::sync::mpsc::Sender<WorkerMsg>,
    rx2: std::sync::mpsc::Receiver<GuiMsg>,
) {
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        // 构建一个发送HTTP请求的客户端
        let client = reqwest::Client::builder().build().unwrap();

        // Communication with the event thread
        let (broadcast_tx, _) = broadcast::channel(1);
        let mut event_rx = None;

        let mut connected = false;

        'main: loop {
            // 检查是否能连接到服务器（本地的8082端口）
            match client.get(BASE_URL).send().await {
                Ok(_) => {
                    // When successfully connected let's (re)create the event stream
                    if !connected {
                        let (event_tx, _event_rx) = mpsc::channel::<alvr_events::Event>(1);
                        // 生成监听是否接收到事件的线程
                        tokio::task::spawn(websocket_task(
                            url::Url::parse(&format!("{}/api/events", BASE_WS_URL)).unwrap(),
                            event_tx,
                            broadcast_tx.subscribe(),
                        ));
                        // 存放负责事件流的mpsc通道接收端
                        event_rx = Some(_event_rx);
                        // 通过mpsc通道发送连接成功的消息
                        let _ = tx1.send(WorkerMsg::Connected);
                        connected = true;
                    }
                }
                Err(why) => {
                    // 如果连接失败，就发送连接失败的消息
                    let _ = broadcast_tx.send(());
                    connected = false;

                    // We still check for the exit signal from the Gui thread
                    // 接收来自GUI线程的退出消息
                    for msg in rx2.try_iter() {
                        if let GuiMsg::Quit = msg {
                            break 'main;
                        }
                    }
                    // 向worker线程发送连接失败的消息
                    let _ = tx1.send(WorkerMsg::LostConnection(format!("{}", why)));
                }
            }

            // If we are not connected, don't even attempt to continue normal working order
            if !connected {
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }

            loop {
                match event_rx.as_mut().unwrap().try_recv() {
                    // 接收到事件，就发送给worker线程
                    Ok(event) => {
                        let _ = tx1.send(WorkerMsg::Event(event));
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(why) => {
                        println!("Error receiving event from event worker: {}", why);
                        break;
                    }
                }
            }

            for msg in rx2.try_iter() {
                // 接收到GUI线程的消息，就调用handle_msg处理
                // 在handle_msg中会通过传入的client引用发送HTTP请求，然后处理返回的结果
                match gui_handler::handle_msg(msg, &client, &tx1).await {
                    Ok(quit) => {
                        // 如果是返回需要quit的消息，就退出
                        if quit {
                            break 'main;
                        }
                    }
                    Err(why) => {
                        let _ = broadcast_tx.send(());
                        connected = false;
                        let _ = tx1.send(WorkerMsg::LostConnection(format!("{}", why)));
                    }
                }
            }
            // With each iteration we should sleep to not consume a thread fully
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        // Shutdown the event thread if needed, an error would only mean that the event thread is already dead so we ignore it
        let _ = broadcast_tx.send(());
    });
}
async fn websocket_task(
    url: url::Url,
    sender: tokio::sync::mpsc::Sender<Event>,
    mut recv: tokio::sync::broadcast::Receiver<()>,
) {
    // Connect to the event stream, and split it so we can get only the read stream
    // 建立socket连接，但是只需要读取流的部分
    let (event_stream, _) = connect_async(url).await.unwrap();
    let (_, event_read) = event_stream.split();

    // The select macro is used to cancel the event task if a shutdown signal is received
    tokio::select! {
        // 循环监听事件流
        _ = event_read.for_each(|msg| async {
            match msg {
                Ok(
                tungstenite::Message::Binary(data)) => {
                    // 监听到事件，反序列化后获得event对象
                    let event = bincode::deserialize(&data).unwrap();

                    // 通过mpsc通道发送事件
                    match sender.send(event).await {
                        Ok(_) => (),
                        Err(why) => {
                            println!("Error sending event: {}", why);
                        }
                    }
                }
                Ok(_) => (),
                Err(why) => println!("Error receiving event: {}", why),
            }
        }) => {},
        // 如果收到了关闭信号会执行这里，关闭任务
        _ = recv.recv() => {},
    };
}
