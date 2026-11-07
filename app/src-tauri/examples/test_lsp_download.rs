// 测试 LSPosed 下载（含代理探测）
fn main() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        // 探测代理
        match droidbox_lib::proxy::detect() {
            Some(p) => println!("探测到代理: {p}"),
            None => println!("未探测到代理（直连可能很慢）"),
        }

        let mgr = droidbox_lib::packages::list_lsposed().await;
        let latest = mgr
            .versions
            .iter()
            .find(|v| v.variant == "unified")
            .expect("未找到最新版");

        println!("\n准备下载: v{} ({})", latest.version, latest.size);
        println!("URL: {}\n", latest.url);

        let start = std::time::Instant::now();
        let result = droidbox_lib::packages::download_lsposed(
            &latest.version,
            &latest.variant,
            &latest.url,
            &latest.asset_name,
            false,
            |p| {
                if p.percent > 0.0 {
                    print!("\r  进度: {:.1}%  {}", p.percent, p.message);
                    use std::io::Write;
                    std::io::stdout().flush().ok();
                }
            },
        )
        .await;

        println!();
        match result {
            Ok(msg) => {
                println!("✓ {msg}");
                println!("耗时: {:.1} 秒", start.elapsed().as_secs_f64());

                // 检查文件
                let dir = droidbox_lib::packages::lsposed_dir();
                if dir.exists() {
                    for e in walkdir::WalkDir::new(&dir).into_iter().flatten() {
                        if e.file_type().is_file() {
                            let size = e.metadata().map(|m| m.len()).unwrap_or(0);
                            println!(
                                "  {} ({:.2} MB)",
                                e.path().display(),
                                size as f64 / 1024.0 / 1024.0
                            );
                        }
                    }
                }
            }
            Err(e) => println!("✗ 失败: {e}"),
        }
    });
}
