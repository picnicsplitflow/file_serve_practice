use anyhow::Result;
use axum::{
    error_handling::HandleError,
    http::{Request, Response, StatusCode},
    middleware::Next,
    response::{Html, IntoResponse},
    routing, Router,
};
use chrono::{DateTime, Local};
use http_body::{combinators::UnsyncBoxBody, Body};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    ffi::OsString,
    fs::{self, read_dir},
    net::SocketAddr,
    ops::Deref,
    path::Path,
    time::SystemTime,
};
use tower_http::services::fs::ServeDir;

#[derive(Serialize, Deserialize, Default)]
struct PsffsSettingJson {
    static_file_serve_url: PathString,
    target_dirs_and_urls: HashMap<PathString, PathString>,
}

///ただのString。パスであることを明示。
#[derive(Serialize, Deserialize, PartialEq, Eq, Hash, Debug, Clone, Default)]
struct PathString(String);

impl std::fmt::Display for PathString {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}
impl From<String> for PathString {
    fn from(string: String) -> Self {
        Self(string)
    }
}
impl Deref for PathString {
    type Target = String;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl AsRef<Path> for &PathString {
    fn as_ref(&self) -> &Path {
        Path::new(self.as_str())
    } //よくわからん
}

#[tokio::main]
async fn main() {
    const SETTINGS_JSON: &str = "psffs_settings.json";

    let setting_json = match read_settings_json(&PathString(SETTINGS_JSON.to_string())) {
        Ok(json) => json,
        Err(e) => {
            eprintln!("ファイルの読み込みでエラーが発生しました:\n{SETTINGS_JSON}:: \n{e}");
            PsffsSettingJson {
                static_file_serve_url: PathString("static".to_string()),
                ..Default::default()
            }
        }
    };

    let (dirs_urls, static_url) = (
        setting_json.target_dirs_and_urls.clone(),
        setting_json.static_file_serve_url.clone(),
    );
    let serve_dir = multi_dirs_fs_router(&setting_json.target_dirs_and_urls).route_layer(
        axum::middleware::from_fn(move |req, next| {
            generate_dir_index_middleware(req, next, dirs_urls.clone(), static_url.clone())
        }),
    );

    let ping = Router::new().route("/ping", routing::get(get_test));

    let addr = SocketAddr::from(([192, 168, 0, 12], 6565)); // 仮

    let app = Router::new()
        .nest(&setting_json.static_file_serve_url, serve_dir)
        .merge(ping);

    dbg!(&app);

    let server = axum::Server::bind(&addr).serve(app.into_make_service());

    let graceful = server.with_graceful_shutdown(shutdown_signal());
    println!("\n終了するには [Ctrl+C] を押してください");

    if let Err(e) = graceful.await {
        eprintln!("エラーによってサーバーが停止しました：{e}");
    }
}
//////// mainここまで

fn read_settings_json(path: &PathString) -> Result<PsffsSettingJson> {
    match fs::read_to_string(&path) {
        Ok(f) => Ok(serde_json::from_str(&f)?),
        Err(e) => {
            println!("{}が見つかりません：{:?}", &path, e);
            Err(e.into())
        }
    }
}

fn multi_dirs_fs_router(dir_path_and_name: &HashMap<PathString, PathString>) -> Router {
    let mut router = Router::new();

    for (path, name) in dir_path_and_name {
        router = router.nest(
            format!("/{name}").as_str(),
            HandleError::new(
                ServeDir::new(path).append_index_html_on_directories(false),
                handle_e,
            ),
        );
        println!("added fs routing: /{name} → {path}");
    } //これ関数的にやりたい
    router
}

async fn get_test() -> Html<&'static str> {
    Html("<h1>alive</h1>")
}

async fn handle_e(err: std::io::Error) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, format!("{err}"))
}

///Ctrl+Cを受け取る。
async fn shutdown_signal() {
    tokio::signal::ctrl_c()
        .await
        .expect("Ctrl + Cコマンドを処理するhandlerのインストールに失敗しました。");
}

///ディレクトリのパスにアクセスした際、indexを返す
async fn generate_dir_index_middleware<B>(
    req: Request<B>,
    next: Next<B>,
    dir_path_name_pair: HashMap<PathString, PathString>,
    static_file_serve_url: PathString,
) -> impl IntoResponse {
    println!("\n\nentry\n");
    dbg!(&req.uri(), &static_file_serve_url);
    let uri = dbg!(req.uri().path().to_string());

    // let uri = dbg!(req.extensions().get::<axum::extract::OriginalUri>().unwrap().0.path().to_string());

    match next.run(req).await {
        not_found if not_found.status() == StatusCode::NOT_FOUND => {
            println!("\nhere!!: from {uri}\n");
            dbg!(&not_found);

            let (target_dir_path_base, _) =
                dir_path_name_pair
                    .keys()
                    .fold((PathString::default(), 0), |re, key| {
                        let v = dbg!(dir_path_name_pair.get(key).unwrap().trim_matches('/'));

                        if !uri.trim_start_matches('/').starts_with(v) {
                            return re;
                        }
                        if re.1 < v.len() {
                            return (key.clone(), v.len());
                        }
                        re
                    });
            dbg!(&target_dir_path_base, &dir_path_name_pair);

            if target_dir_path_base == PathString::default() {
                dbg!("!不明なエラー"); //ここには来ないはず。
                return not_found;
            }
            let uri = percent_encoding::percent_decode_str(&uri)
                .decode_utf8_lossy()
                .to_string();
            //dirの中身を取得していく
            let dir_path = std::path::PathBuf::from(dbg!(format!(
                //対象(子)ディレクトリの絶対パスを構成
                "{}{}",
                target_dir_path_base,
                uri.trim_start_matches(
                    format!(
                        "/{}",
                        dir_path_name_pair.get(&target_dir_path_base).unwrap()
                    )
                    .as_str()
                )
            )));

            let dir_uri = uri_encode(&format!("{}{}", static_file_serve_url, uri));

            let items_meta = read_dir_items_for_index(&dir_path);
            let entry_table = build_entries_li(items_meta, &dir_uri);
            // match std::fs::read_dir(dir_path) {
            //     Ok(dir) => {
            //         dbg!(&dir);
            //         dir.for_each(|i| {
            //             //ディレクトリ内のアイテム一覧
            //             // dbg!(&i);
            //             // dbg!(&i.unwrap().metadata().unwrap());
            //         });
            //     }
            //     Err(e) => {
            //         dbg!(e);
            //     }
            // }

            //レスポンスを構成
            let builder = Response::builder();
            dbg!(builder
                .status(StatusCode::OK)
                .body(
                    http_body::Full::from(axum::body::Bytes::from(format!(
                        "<!DOCTYPE html><html><head><meta charset=\"utf-8\"></head><body><h1><a href={}>{}</a></h1> <p>{}</p>{}</body></html>",
                        uri_encode(&format!("{}{}", static_file_serve_url, uri)),
                        uri,
                        target_dir_path_base,
                        entry_table
                    )))
                    .map_err(|err| match err {})
                    .boxed_unsync(),
                )
                .expect("index.htmlの生成でエラー"))
        }
        tmp_red307 if tmp_red307.status() == StatusCode::TEMPORARY_REDIRECT => {
            match dbg!(tmp_red307.headers().get("location")) {
                Some(location) => {
                    // ベースのURLが切られて返ってくるので、元に戻す
                    // 末尾の一致を確認して結合

                    if !uri.ends_with(location.to_str().unwrap().trim_matches('/'))
                        || !location.to_str().unwrap().ends_with('/')
                    {
                        dbg!("unknown / unimplemented redirect occurred"); //想定外のケースを通知
                        return tmp_red307;
                    }

                    let builder = Response::builder();
                    return dbg!(builder
                        .status(StatusCode::TEMPORARY_REDIRECT)
                        .header("location", format!("{}{}/", static_file_serve_url, uri)) //末尾にスラッシュ追加
                        .body(UnsyncBoxBody::default())
                        .expect("msg"));
                }
                _ => {
                    dbg!("! redirect handler error: header 'location' is missing");
                    dbg!(tmp_red307)
                }
            }
        }
        ok200 if ok200.status() == StatusCode::OK => return ok200,
        other => {
            dbg!(other)
        }
    }
}

fn build_entries_li(items_meta: Result<Vec<DirEntryMetadata>>, uri_base: &String) -> String {
    let mut s = String::new();
    if let Err(e) = items_meta {
        return s;
    }
    s.push_str("<table><thead><tr><th>name</th><th>size</th><th>created</th></tr></thead><tbody>");
    let mut meta_v = items_meta.unwrap();
    meta_v.sort_by_cached_key(|k| k.created);
    meta_v.reverse();

    for meta in meta_v {
        s.push_str(&format!(
            "<tr><td><a href={}>{}</a></td><td>{}</td><td>{}</td></tr>",
            format!(
                "{}{}",
                uri_base,
                percent_encoding::utf8_percent_encode(&meta.name.to_string_lossy(), &CHAR_SET)
            ),
            meta.name.to_string_lossy(),
            meta.len,
            meta.created
        ))
    }
    s.push_str("</tbody></table>");
    s
}

const CHAR_SET: percent_encoding::AsciiSet = percent_encoding::NON_ALPHANUMERIC
    .remove(b'_')
    .remove(b'/')
    .remove(b'\\')
    .remove(b'.')
    .remove(b'#')
    .remove(b'`')
    .remove(b'{')
    .remove(b'}');

fn uri_encode(uri: &str) -> String {
    percent_encoding::utf8_percent_encode(uri, &CHAR_SET).to_string()
}

struct DirEntryMetadata {
    name: OsString,
    is_file: bool,
    len: u64,
    created: DateTime<Local>,
}

fn read_dir_items_for_index(dir_path: &std::path::Path) -> Result<Vec<DirEntryMetadata>> {
    let mut v = Vec::<_>::new();
    let rd = read_dir(dir_path)?;
    for i in rd {
        let item = match i {
            Ok(entry) => {
                entry.metadata().unwrap();
                if let Ok(meta) = entry.metadata() {
                    DirEntryMetadata {
                        name: entry.file_name(),
                        is_file: meta.is_file(),
                        len: meta.len(),
                        created: DateTime::<Local>::from(meta.created().unwrap()), //unwrapよくない
                    }
                } else {
                    println!(
                        "file can not loaded! :[{}]",
                        entry.file_name().to_string_lossy()
                    );
                    continue;
                }
            }
            Err(e) => {
                println!("{}", e);
                continue;
            }
        };
        v.push(item);
    }

    Ok(v)
}

#[cfg(test)]
mod test {
    use crate::*;
}
