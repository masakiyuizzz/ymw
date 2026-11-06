use std::sync::mpsc::RecvTimeoutError;
use std::time::{Duration, Instant};

use anyhow::Result;

use crate::cli::Args;
use crate::engine::{self, Cmd, Ev, PointerKind};

fn data_url() -> String {
    let html = r#"<html><head><meta charset="utf-8"><style>
    body{background:#111;color:#e8e8e8;font:22px -apple-system;margin:40px}
    #anim{width:60px;height:60px;background:#e9b46a;margin-top:30px;
          animation:pulse 0.6s infinite alternate}
    @keyframes pulse{from{opacity:.15}to{opacity:1}}
    </style></head><body>
    <p id="t">SELECTABLE TEXT FOR THE DRAG TEST</p>
    <div id="anim"></div>
    </body></html>"#;
    let mut s = String::from("data:text/html;charset=utf-8,");
    for b in html.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                s.push(b as char)
            }
            _ => s.push_str(&format!("%{b:02X}")),
        }
    }
    s
}

pub fn run(args: &Args) -> Result<()> {
    let engine_bin = engine::chrome::find(args.engine.as_deref())
        .ok_or_else(|| anyhow::anyhow!("no engine found"))?;
    println!("engine    {}", engine_bin.path.display());

    let cfg = engine::Config {
        engine: engine_bin,
        profile: args
            .profile
            .clone()
            .unwrap_or_else(engine::chrome::default_profile)
            .join("selftest"),
        user_agent: None,
        extra_args: args.engine_arg.clone(),
        no_sandbox: args.no_sandbox,
        search: "https://www.bing.com/search?q=%s".into(),
        zoom: 1.0,
        scale: 1.0,
        fps: 15,
        raw_frames: false,
    };

    let start = Instant::now();
    let h = engine::start(cfg)?;
    println!("startup   {:?}  ({})", start.elapsed(), h.info.product);

    let target_url = if args.target.trim().is_empty() {
        data_url()
    } else {
        args.target.clone()
    };
    let probing_video = !args.target.trim().is_empty();

    h.tx.send(Cmd::Viewport { w: 960, h: 640 })?;
    h.tx.send(Cmd::NewTab(Some(target_url)))?;
    let mut target: Option<String> = None;

    // 若是视频页，先点一下让它开播，再统计帧
    if probing_video {
        let mut waited = 0;
        while target.is_none() && waited < 40 {
            match h.rx.recv_timeout(Duration::from_millis(100)) {
                Ok(Ev::Created { target: t, .. }) => target = Some(t),
                Ok(_) => {}
                Err(_) => {}
            }
            waited += 1;
        }
        if let Some(t) = target.clone() {
            std::thread::sleep(Duration::from_millis(700));
            // 只有在暂停时才点击：有些页面本来就能自动播放，点一下反而会暂停
            h.tx.send(Cmd::Eval {
                target: t.clone(),
                js: "(()=>{const v=document.querySelector('video');return v?v.paused:false})()".into(),
            })?;
            let mut paused = true;
            let deadline = Instant::now() + Duration::from_secs(2);
            while Instant::now() < deadline {
                match h.rx.recv_timeout(Duration::from_millis(150)) {
                    Ok(Ev::Eval { value, .. }) => {
                        paused = value.as_bool().unwrap_or(true);
                        break;
                    }
                    Ok(_) => {}
                    Err(_) => break,
                }
            }
            if paused {
                h.tx.send(Cmd::Pointer {
                    target: t.clone(),
                    x: 320.0,
                    y: 180.0,
                    kind: PointerKind::Down,
                })?;
                std::thread::sleep(Duration::from_millis(80));
                h.tx.send(Cmd::Pointer {
                    target: t,
                    x: 320.0,
                    y: 180.0,
                    kind: PointerKind::Up,
                })?;
                std::thread::sleep(Duration::from_millis(400));
            }
        }
    }

    let mut frames = 0usize;
    let mut first_frame = None;
    let mut frame_bytes = 0usize;
    let want_frames = std::env::var("YMW_SELFTEST_FRAMES")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(8);
    let deadline = Instant::now()
        + Duration::from_secs(if probing_video {
            want_frames.min(60) as u64 / 3 + 6
        } else {
            6
        });
    while Instant::now() < deadline {
        match h.rx.recv_timeout(Duration::from_millis(200)) {
            Ok(Ev::Created { target: t, .. }) => {
                target = Some(t);
                println!("tab       created");
            }
            Ok(Ev::Frame { frame, .. }) => {
                if first_frame.is_none() {
                    first_frame = Some(start.elapsed());
                }
                frames += 1;
                frame_bytes += match &frame {
                    crate::graphics::Frame::Png(b) => b.len(),
                    crate::graphics::Frame::Rgb { data, .. } => data.len(),
                };
                if frames >= want_frames {
                    break;
                }
            }
            Ok(Ev::Error(e)) => println!("error     {e}"),
            Ok(_) => {}
            Err(RecvTimeoutError::Timeout) => continue,
            Err(_) => break,
        }
    }
    let stream_secs = start.elapsed().as_secs_f64();
    println!(
        "screencast {} frames in {:.1}s ({:.1} fps, first at {:?}, avg {} KB) — live: {}",
        frames,
        stream_secs,
        frames as f64 / stream_secs.max(0.01),
        first_frame.unwrap_or_default(),
        frame_bytes / frames.max(1) / 1024,
        frames >= 5
    );

    let Some(target) = target else {
        println!("no tab was created");
        return Ok(());
    };

    if probing_video {
        let mut samples = Vec::new();
        for _ in 0..12 {
            h.tx.send(Cmd::Eval {
                target: target.clone(),
                js: "(()=>{const v=document.querySelector('video');                     return v?JSON.stringify([v.currentTime,v.paused,v.readyState,v.videoWidth]):'none'})()"
                    .into(),
            })?;
            let deadline = Instant::now() + Duration::from_secs(2);
            while Instant::now() < deadline {
                match h.rx.recv_timeout(Duration::from_millis(150)) {
                    Ok(Ev::Eval { value, .. }) => {
                        samples.push(value.to_string());
                        break;
                    }
                    Ok(_) => {}
                    Err(_) => break,
                }
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        println!("video samples {}", samples.join(" "));
        let times: Vec<f64> = samples
            .iter()
            .filter_map(|s| {
                let clean = s.trim_matches('"').replace("\\\"", "\"");
                let v: serde_json::Value = serde_json::from_str(&clean).ok()?;
                v.get(0)?.as_f64()
            })
            .collect();
        let monotonic = times.windows(2).all(|w| w[1] >= w[0]);
        println!(
            "video       {} samples, t: {:?} -> {:?}, monotonic: {}",
            times.len(),
            times.first().copied().unwrap_or(0.0),
            times.last().copied().unwrap_or(0.0),
            monotonic
        );
    }

    // drag across the paragraph and copy the selection
    h.tx.send(Cmd::Pointer {
        target: target.clone(),
        x: 40.0,
        y: 45.0,
        kind: PointerKind::Down,
    })?;
    std::thread::sleep(Duration::from_millis(120));
    h.tx.send(Cmd::Pointer {
        target: target.clone(),
        x: 420.0,
        y: 45.0,
        kind: PointerKind::Move,
    })?;
    std::thread::sleep(Duration::from_millis(120));
    h.tx.send(Cmd::Pointer {
        target: target.clone(),
        x: 420.0,
        y: 45.0,
        kind: PointerKind::Up,
    })?;
    std::thread::sleep(Duration::from_millis(200));
    h.tx.send(Cmd::CopySelection(target.clone()))?;

    let mut copied = String::new();
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        match h.rx.recv_timeout(Duration::from_millis(200)) {
            Ok(Ev::Clipboard { text }) => {
                copied = text;
                break;
            }
            Ok(_) => {}
            Err(RecvTimeoutError::Timeout) => continue,
            Err(_) => break,
        }
    }
    // audio: a real user gesture has just happened (the click above), so an
    // AudioContext is allowed to start. Its state tells us whether CoreAudio
    // is reachable from the headless browser.
    h.tx.send(Cmd::Eval {
        target: target.clone(),
        js: "(async()=>{const c=new AudioContext();const s=c.state;await c.close();return s})()".into(),
    })?;
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        match h.rx.recv_timeout(Duration::from_millis(200)) {
            Ok(Ev::Eval { value, .. }) => {
                println!("audio     AudioContext state = {value}");
                break;
            }
            Ok(_) => {}
            Err(RecvTimeoutError::Timeout) => continue,
            Err(_) => break,
        }
    }

    println!("selection {:?}", copied.trim());
    println!("drag select works: {}", copied.contains("SELECTABLE"));

    h.tx.send(Cmd::Shutdown)?;
    Ok(())
}
