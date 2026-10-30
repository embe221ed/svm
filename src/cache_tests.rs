use super::{GITHUB_API, ReleaseCache, download_archive, read_release_cache};
use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;
use std::time::{Duration, Instant};

#[test]
fn legacy_metadata_cache_is_only_readable_by_official_api() {
    let tmp = tempfile::TempDir::new().unwrap();
    fs::create_dir(tmp.path().join("cache")).unwrap();
    let legacy = ReleaseCache {
        etag: Some("\"legacy\"".into()),
        pages: 1,
        releases: vec![serde_json::json!({"tag_name": "mainnet-v1.2.3"})],
    };
    fs::write(
        tmp.path().join("cache/releases.json"),
        serde_json::to_vec(&legacy).unwrap(),
    )
    .unwrap();
    assert_eq!(
        read_release_cache(tmp.path(), GITHUB_API).unwrap().releases,
        legacy.releases
    );
    assert!(read_release_cache(tmp.path(), "https://mirror.example/releases").is_none());
}

#[test]
fn concurrent_downloads_publish_complete_archives_with_independent_partial_files() {
    let tmp = tempfile::TempDir::new().unwrap();
    let archive = tmp.path().join("release.tgz");
    let mut servers = Vec::new();
    let mut downloads = Vec::new();
    let mut release_transfers = Vec::new();

    for body in [b"first archive".as_slice(), b"second archive".as_slice()] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (release, wait) = mpsc::channel();
        release_transfers.push(release);
        servers.push(std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                stream.read_exact(&mut byte).unwrap();
                request.push(byte[0]);
            }
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .unwrap();
            stream.flush().unwrap();
            wait.recv_timeout(Duration::from_secs(5)).unwrap();
            stream.write_all(body).unwrap();
        }));
        let dest = archive.clone();
        downloads.push(std::thread::spawn(move || {
            let client = reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(5))
                .build()
                .unwrap();
            download_archive(&client, &url, &dest, "release")
        }));
    }

    let deadline = Instant::now() + Duration::from_secs(3);
    let partial_count = loop {
        let count = fs::read_dir(tmp.path()).unwrap().count();
        if count == 2 || Instant::now() >= deadline {
            break count;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    assert!(
        !archive.exists(),
        "incomplete transfers must not be published"
    );
    for release in release_transfers {
        release.send(()).unwrap();
    }
    for download in downloads {
        download.join().unwrap().unwrap();
    }
    for server in servers {
        server.join().unwrap();
    }
    assert_eq!(partial_count, 2, "each transfer needs its own partial file");
    let contents = fs::read(archive).unwrap();
    assert!(contents == b"first archive" || contents == b"second archive");
    assert_eq!(
        fs::read_dir(tmp.path()).unwrap().count(),
        1,
        "partial files must be cleaned up"
    );
}

#[test]
fn interrupted_download_preserves_existing_archive_and_removes_partial() {
    let tmp = tempfile::TempDir::new().unwrap();
    let archive = tmp.path().join("release.tgz");
    fs::write(&archive, b"previous complete archive").unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0; 4096];
        assert!(stream.read(&mut request).unwrap() > 0);
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Length: 1000\r\nConnection: close\r\n\r\ntruncated",
            )
            .unwrap();
    });
    let client = reqwest::blocking::Client::new();
    assert!(download_archive(&client, &url, &archive, "release").is_err());
    server.join().unwrap();
    assert_eq!(fs::read(archive).unwrap(), b"previous complete archive");
    assert_eq!(fs::read_dir(tmp.path()).unwrap().count(), 1);
}
