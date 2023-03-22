use alvr_dashboard::dashboard::{DashboardResponse, DriverResponse, FirewallRulesResponse};

use crate::{GuiMsg, WorkerMsg};

use super::BASE_URL;

pub async fn handle_msg(
    msg: GuiMsg,
    client: &reqwest::Client,
    tx1: &std::sync::mpsc::Sender<WorkerMsg>,
) -> reqwest::Result<bool> {
    Ok(match msg {
        // 对应的api接口参考server/src/web_server.rs
        // 退出
        GuiMsg::Quit => true,

        // 获取会话设置
        GuiMsg::GetSession => {
            // 读取会话设置（json格式）
            let response = client
                .get(format!("{}/api/session/load", BASE_URL))
                .send()
                .await?;

            // 转为SessionDesc结构体
            let session = match response.json::<alvr_session::SessionDesc>().await {
                Ok(session) => session,
                Err(why) => {
                    println!("Error parsing session JSON: {}", why);
                    // Err returns are reserved for connectivity errors
                    return Ok(false);
                }
            };

            // Discarded as the receiving end will always be valid, and when it is not the dashboard is shutting down anyway
            // 向worker线程发送会话设置，接收端放在GUI里面
            let _ = tx1.send(WorkerMsg::SessionResponse(session));
            false
        }

        // 获取驱动列表
        GuiMsg::GetDrivers => {
            get_drivers(client, tx1).await?;
            false
        }

        // Dashboard相关的命令
        GuiMsg::Dashboard(response) => match response {
            DashboardResponse::SessionUpdated(session) => {
                let text = serde_json::to_string(&session).unwrap();
                client
                    .get(format!("{}/api/session/store", BASE_URL))
                    .body(format!("{{\"session\": {}}}", text))
                    .send()
                    .await?;
                false
            }
            DashboardResponse::RestartSteamVR => {
                client
                    .get(format!("{}/restart-steamvr", BASE_URL))
                    .send()
                    .await?;
                false
            }
            DashboardResponse::Driver(driver) => match driver {
                DriverResponse::RegisterAlvr => {
                    client
                        .get(format!("{}/api/driver/register", BASE_URL))
                        .send()
                        .await?;

                    get_drivers(client, tx1).await?;
                    false
                }
                DriverResponse::Unregister(path) => {
                    client
                        .get(format!("{}/api/driver/unregister", BASE_URL))
                        .body(format!(r#""{}""#, path))
                        .send()
                        .await?;
                    get_drivers(client, tx1).await?;
                    false
                }
            },
            DashboardResponse::Firewall(firewall) => match firewall {
                FirewallRulesResponse::Add => {
                    client
                        .get(format!("{}/api/firewall-rules/add", BASE_URL))
                        .send()
                        .await?;
                    false
                }
                FirewallRulesResponse::Remove => {
                    client
                        .get(format!("{}/api/firewall-rules/remove", BASE_URL))
                        .send()
                        .await?;
                    false
                }
            },
            _ => false,
        },
    })
}

// Some functions to reduce code duplication
async fn get_drivers(
    client: &reqwest::Client,
    tx1: &std::sync::mpsc::Sender<WorkerMsg>,
) -> reqwest::Result<()> {
    let response = client
        .get(format!("{}/api/driver/list", BASE_URL))
        .send()
        .await?;

    let vec: Vec<String> = match response.json().await {
        Ok(vec) => vec,
        Err(why) => {
            println!("Error parsing driver list JSON: {}", why);
            // We return Ok(()) here as an Err variant is used to signal being offline
            return Ok(());
        }
    };

    // If this errors out, the GUI thread has already exited anyway and the worker will as well so it is safe to discard the error
    let _ = tx1.send(WorkerMsg::DriverResponse(vec));

    Ok(())
}
