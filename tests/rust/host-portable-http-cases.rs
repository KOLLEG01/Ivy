use super::*;
use ivy_host_runtime::portable_http::PortableHttp;
use std::{io::{BufRead,BufReader},process::{Child,Output}};

struct Server { child: Child, metadata: IvyJson }
impl Server {
    fn new(archive:Option<&Path>)->Self{
        let mut command=Command::new("node");command.arg("tests/fixtures/host-portable-http-server.mjs").current_dir(root())
            .stdout(Stdio::piped()).stderr(Stdio::piped()).env_remove("NODE_OPTIONS");
        if let Some(archive)=archive{command.arg(archive);}
        let mut child=command.spawn().unwrap();let mut line=String::new();BufReader::new(child.stdout.take().unwrap()).read_line(&mut line).unwrap();
        let metadata=json::parse(&line,usize::MAX).unwrap();let version=metadata.get("node").unwrap().as_str().unwrap();let version=version.split('.').map(|v|v.parse::<u32>().unwrap()).collect::<Vec<_>>();assert!(version[0]==24&&version[1]>=18);assert!(metadata.get("undici").is_some());
        Self{child,metadata}
    }
    fn url(&self,secure:bool,actor:&str,case:&str)->JsString{format!("{}/{actor}/{case}",self.metadata.get(if secure{"https"}else{"http"}).unwrap().as_str().unwrap()).into()}
    fn plain(&self)->Vec<u8>{STANDARD.decode(self.metadata.get("plain").unwrap().as_str().unwrap()).unwrap()}
    fn client(&self,trusted:bool)->PortableHttp{
        let certs=if trusted{vec![reqwest::Certificate::from_pem(&fs::read(root().join("tests/fixtures/host-portable-http-ca.pem")).unwrap()).unwrap()]}else{vec![]};
        PortableHttp::new(reqwest::Client::builder().no_proxy().tls_certs_only(certs)).unwrap()
    }
    async fn stats(&self)->IvyJson{
        let url=format!("{}/stats",self.metadata.get("http").unwrap().as_str().unwrap());
        let bytes=self.client(true).fetch(&url.into()).await.unwrap();json::parse(std::str::from_utf8(&bytes).unwrap(),usize::MAX).unwrap()
    }
}
impl Drop for Server {fn drop(&mut self){let _=self.child.kill();let _=self.child.wait();}}
fn start_original(input:&IvyJson,trusted:bool)->Child{
    let mut command=Command::new("node");command.args(["--experimental-transform-types","--disable-warning=ExperimentalWarning","tests/fixtures/host-portable-dependencies-oracle.mjs"])
        .current_dir(root()).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).env_remove("NODE_OPTIONS").env_remove("NODE_TLS_REJECT_UNAUTHORIZED");
    if trusted{command.env("NODE_EXTRA_CA_CERTS",root().join("tests/fixtures/host-portable-http-ca.pem"));}else{command.env_remove("NODE_EXTRA_CA_CERTS");}
    let mut child=command.spawn().unwrap();child.stdin.take().unwrap().write_all(json::stringify(input,usize::MAX).unwrap().as_bytes()).unwrap();child
}
fn finish_original(output:Output)->IvyJson{assert!(output.status.success(),"{}",String::from_utf8_lossy(&output.stderr));json::parse(std::str::from_utf8(&output.stdout).unwrap(),usize::MAX).unwrap()}
fn retain(input:IvyJson,actual:&IvyJson,expected:&IvyJson,server:&Server){
    if let Some(file)=std::env::var_os("IVY_HOST_PORTABLE_CORPUS"){let _guard=CORPUS.lock().unwrap();writeln!(fs::OpenOptions::new().create(true).append(true).open(file).unwrap(),"{}",json::stringify(&IvyJson::object([("input",input),("actual",actual.clone()),("expected",expected.clone()),("server",server.metadata.clone())]),usize::MAX).unwrap()).unwrap();}
}
async fn download(server:&Server,secure:bool,case:&str,expected_bytes:&[u8],trusted:bool)->IvyJson{
    let mut dep=dependency("node_modules/fixture",expected_bytes);dep.resolved=server.url(secure,"original",case);
    let input=IvyJson::object([("operation","download".into()),("dependency",value(&dep))]);let original=start_original(&input,trusted);
    dep.resolved=server.url(secure,"actual",case);
    let result=async{
        let bytes=server.client(trusted).fetch(&dep.resolved).await?;portable::verify_portable_bytes(&dep,&bytes)?;
        Ok(IvyJson::object([("byteLength",IvyJson::Number(bytes.len() as f64)),("sha256",ivy_contracts::hash::digest(&bytes)[7..].into())]))
    }.await;
    let actual=wire(result);let expected=finish_original(original.wait_with_output().unwrap());retain(input,&actual,&expected,server);
    assert_eq!(&actual,expected.get("operation").unwrap(),"HTTP fixture {case}");actual
}

#[tokio::test]
async fn native_http_decodes_original_encodings_chains_truncation_and_refusals(){
    let server=Server::new(None);let plain=server.plain();
    for case in ["plain","gzip","x-gzip","upper","zlib","raw","br","zstd","chain","duplicates","five","unknown","mixed-unknown","identity","truncated-gzip","truncated-zlib","gzip-zero-padding","gzip-zero-garbage","split-gzip","split-raw"]{
        succeeds(&download(&server,true,case,&plain,true).await);
    }
    succeeds(&download(&server,true,"gzip-concat",&plain.repeat(2),true).await);
    succeeds(&download(&server,true,"zstd-concat",&plain,true).await);
    for case in ["six","six-error","bad-crc","bad-zstd","gzip-garbage","nested-bad-crc","late-zstd","late-br"]{refuses(&download(&server,true,case,&plain,true).await,"internal_error");}
    for case in ["empty","gzip-header","truncated-raw","truncated-br","truncated-zstd","no-content"]{download(&server,true,case,&plain,true).await;}
    refuses(&download(&server,true,"status",&plain,true).await,"service_unavailable");
    refuses(&download(&server,true,"large-declared",&plain,true).await,"content_too_large");
    refuses(&download(&server,true,"plain",b"different checksum",true).await,"artifact_changed");
}

#[tokio::test]
async fn native_http_preserves_twenty_redirects_credentials_status_and_request_headers(){
    let server=Server::new(None);let plain=server.plain();
    for case in ["relative","downgrade","redirect/20"]{succeeds(&download(&server,true,case,&plain,true).await);}
    for case in ["redirect/21","credential","bad-location"]{refuses(&download(&server,true,case,&plain,true).await,"internal_error");}
    download(&server,true,"duplicate-location",&plain,true).await;
    let stats=server.stats().await;let requests=stats.get("requests").unwrap().as_array().unwrap();
    for actor in ["actual","original"]{
        let values=requests.iter().filter(|value|value.get("actor")==Some(&actor.into())).collect::<Vec<_>>();assert!(!values.is_empty());
        assert!(values.iter().any(|value|value.get("headers").unwrap().get("accept-encoding")==Some(&"gzip, deflate".into())));
        for value in values{let headers=value.get("headers").unwrap();assert_eq!(headers.get("accept"),Some(&"*/*".into()));assert_eq!(headers.get("accept-language"),Some(&"*".into()));assert_eq!(headers.get("user-agent"),Some(&"node".into()));assert_eq!(headers.get("sec-fetch-mode"),Some(&"cors".into()));assert!(headers.get("referer").is_none());assert!(headers.get("authorization").is_none());}
    }
}

#[tokio::test]
async fn native_http_rejects_untrusted_tls_and_bad_urls_without_relaxing_verification(){
    let server=Server::new(None);let plain=server.plain();refuses(&download(&server,true,"plain",&plain,false).await,"internal_error");
    for resolved in ["https://127.0.0.1:1/","https://127.0.0.1:4190/","https://127.0.0.1:6679/","https://user:secret@127.0.0.1/","https://[invalid/","file:///unreachable"]{
        let mut dep=dependency("node_modules/fixture",&plain);dep.resolved=resolved.into();let input=IvyJson::object([("operation","download".into()),("dependency",value(&dep))]);let original=start_original(&input,true);
        let actual=wire(server.client(true).fetch(&dep.resolved).await.map(|_|IvyJson::Null));let expected=finish_original(original.wait_with_output().unwrap());retain(input,&actual,&expected,&server);assert_eq!(&actual,expected.get("operation").unwrap());refuses(&actual,"internal_error");
    }
}

#[tokio::test]
async fn native_http_retains_decoded_size_limit_and_late_transport_failure_order(){
    let server=Server::new(None);let large=vec![97;64*1024*1024];
    succeeds(&download(&server,true,"large",&large,true).await);
    refuses(&download(&server,true,"oversize",&large,true).await,"artifact_changed");
    refuses(&download(&server,true,"late-close",&large,true).await,"internal_error");
}

#[tokio::test]
async fn native_http_deadline_covers_headers_and_body_for_original_120_seconds(){
    let server=Server::new(None);let started=std::time::Instant::now();
    refuses(&download(&server,true,"hang",&server.plain(),true).await,"internal_error");
    assert!(started.elapsed()>=std::time::Duration::from_secs(118));assert!(started.elapsed()<std::time::Duration::from_secs(145));
    let stats=server.stats().await;assert_eq!(stats.get("active"),Some(&IvyJson::Number(0.0)));
}

fn worker_lock(server:&Server,actor:&str,bytes:&[u8],fail:bool)->(IvyJson,Vec<JsString>){
    let mut packages=Vec::new();let mut selected=Vec::new();
    for index in 0..6{
        let location=format!("node_modules/outer-{index}/node_modules/fixture");let mut dep=dependency(&location,bytes);
        dep.resolved=server.url(true,actor,if fail&&index==0{"worker/fail"}else{"worker/ok"});let mut item=value(&dep);item.insert("os",IvyJson::Array(vec!["linux".into()]));
        packages.push((location.clone(),item));selected.push(location.into());
    }
    (IvyJson::object([("packages",IvyJson::object(packages))]),selected)
}

#[tokio::test]
async fn native_http_four_workers_stop_admission_and_drain_fetch_install_cleanup(){
    for fail in [false,true]{
        let fixture=Fixture::new();let bytes=archive(&fixture,vec![("package/package.json",br#"{"name":"fixture","version":"1.2.3"}"#.to_vec(),0o644),("package/payload",b"owned native HTTP payload".to_vec(),0o755)]);
        let server=Server::new(Some(&fixture.0.join("fixture.tgz")));let actual=fixture.0.join("actual");let original=fixture.0.join("original");let actual_temp=fixture.0.join("actual-temp");let original_temp=fixture.0.join("original-temp");
        for directory in [&actual,&original,&actual_temp,&original_temp]{fs::create_dir(directory).unwrap();}
        let (lock,selected)=worker_lock(&server,"original",&bytes,fail);
        let input=IvyJson::object([("operation","materializeDownload".into()),("lock",lock),("selected",IvyJson::Array(selected.into_iter().map(IvyJson::String).collect())),("originalRoot",path(&original)),("temporaryRoot",path(&original_temp)),("executables",IvyJson::object([("tar",path(&tar()))]))]);
        let original_process=start_original(&input,true);let (lock,selected)=worker_lock(&server,"actual",&bytes,fail);
        let answer=wire(portable::materialize_portable_dependencies(lock,selected,actual.clone(),actual_temp.clone(),BTreeMap::from([("tar".into(),tar())]),server.client(true)).await.map(|_|IvyJson::Null));
        let expected=finish_original(original_process.wait_with_output().unwrap());retain(input,&answer,&expected,&server);assert_eq!(&answer,expected.get("operation").unwrap());
        if fail{refuses(&answer,"service_unavailable");}else{succeeds(&answer);}
        for index in 0..6{
            let name=format!("node_modules/outer-{index}/node_modules/fixture/payload");let present=!fail||(1..4).contains(&index);
            assert_eq!(actual.join(&name).exists(),present);assert_eq!(original.join(&name).exists(),present);
            if present{assert_eq!(fs::read(actual.join(&name)).unwrap(),fs::read(original.join(&name)).unwrap());}
        }
        for directory in [&actual_temp,&original_temp]{assert_eq!(fs::read_dir(directory).unwrap().count(),0);}
        let stats=server.stats().await;
        for actor in ["actual","original"]{
            let owner=stats.get("owners").unwrap().get(actor).unwrap();assert_eq!(owner.get("active"),Some(&IvyJson::Number(0.0)));assert_eq!(owner.get("maximum"),Some(&IvyJson::Number(4.0)));
            assert_eq!(stats.get("requests").unwrap().as_array().unwrap().iter().filter(|value|value.get("actor")==Some(&actor.into())).count(),if fail{4}else{6});
        }
        retain(IvyJson::object([("operation","worker-observation".into()),("failure",fail.into())]),&stats,&stats,&server);
    }
}
