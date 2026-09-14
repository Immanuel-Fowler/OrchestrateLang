struct AppHost;

impl scripts::Host for AppHost {
    fn world_record(&self, value: i64) -> Result<i64, String> {
        println!("host received {value}");
        Ok(value)
    }
}

fn main() -> Result<(), String> {
    let runtime = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
    runtime.block_on(async {
        let mut scripts = scripts::start(runtime.handle(), AppHost)?;
        scripts.ready().await?;
        for _ in 0..3 {
            if scripts.stop_requested() { break; }
            scripts.tick(1.0 / 60.0).await?;
        }
        scripts.shutdown().await?;
        println!("the host is still running");
        Ok(())
    })
}
