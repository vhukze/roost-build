// 验证 LSPosed 版本列表的后端实际返回值
fn main() {
    // 手动设置代理
    if std::env::var("ROOST_PROXY").is_err() {
        std::env::set_var("ROOST_PROXY", "http://127.0.0.1:7897");
    }

    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let mgr = droidbox_lib::packages::list_lsposed().await;
        println!("状态: {}", mgr.status);
        println!("版本总数: {}", mgr.versions.len());
        println!();

        println!("{:<12} {:<8} {:<12} {:<10} {}", "版本", "变体", "大小", "日期", "下载地址");
        println!("{}", "-".repeat(90));
        for v in &mgr.versions {
            let url_short = if v.url.chars().count() > 40 {
                format!("{}...", v.url.chars().take(40).collect::<String>())
            } else {
                v.url.clone()
            };
            println!(
                "v{:<11} {:<8} {:<12} {:<10} {}",
                v.version, v.variant, v.size, v.date, url_short
            );
        }
    });
}
