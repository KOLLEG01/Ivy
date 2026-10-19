//! Static preflight for the approved v1 native shims. Candidate JavaScript is
//! never evaluated. Ordinary nonmatching TypeScript entries retain their path.
use crate::{IvyError,IvyJson,IvyResult,config::{read_json,FileError,CONFIGURATION_BYTES}};
use crate::paths::direct_native_file as direct_file;
use std::{fs,io::Read,path::Path};

fn require(value:bool,code:&str,message:&str)->IvyResult<()>{if value{Ok(())}else{Err(IvyError::new(code,message))}}
fn command_path(command:Option<&IvyJson>)->Option<String>{
    let path=command?.get("args")?.as_array()?.first()?.as_str().ok()?;
    if !path.starts_with("dist/")||!path.ends_with(".js")||path.len()<=8
        ||!path[5..].bytes().all(|c|c.is_ascii_alphanumeric()||b"_./-".contains(&c))
        ||path.split('/').any(|part|part=="."||part==".."){return None}Some(path)
}
// Match buildNativeDispatchers' exact byte template. The input alphabet is
// ASCII without quotes, so its JSON string literals have no escaped units.
fn shim(path:&str,name:&str,shutdown:f64,health:bool)->Vec<u8>{
    let mut from=path[5..].split('/').filter(|part|!part.is_empty()).collect::<Vec<_>>();from.pop();
    let to=["native","dispatch.mjs"];let common=from.iter().zip(to).take_while(|(left,right)|**left==*right).count();
    let mut module="../".repeat(from.len()-common);module.push_str(&to[common..].join("/"));if !module.starts_with('.'){module.insert_str(0,"./");}
    let unavailable=if health{"process.stdout.write('{\"ok\":false,\"code\":\"artifact_missing\"}\\n');"}else{"process.stderr.write(\"Native dispatcher is unavailable.\\n\");"};
    format!("import(\"{module}\").then(({{dispatch,dispatchFailure}})=>dispatch(\"{name}\",{shutdown:.0}).catch(error=>dispatchFailure(error,{health}))).catch(()=>{{{unavailable}process.exitCode=1;}});\n").into_bytes()
}
fn matches(path:&Path,expected:&[u8])->bool{
    // Bounded reads can establish intent through a link, which the strict
    // native check below then refuses. Missing or ordinary entries are not JS.
    let Ok(metadata)=fs::metadata(path)else{return false};if !metadata.is_file()||metadata.len()!=expected.len() as u64{return false}
    let Ok(file)=fs::File::open(path)else{return false};let mut bytes=Vec::new();file.take(expected.len() as u64+1).read_to_end(&mut bytes).is_ok()&&bytes==expected
}
pub(crate) fn verify(candidate:&IvyJson,root:&Path,manifest:&IvyJson)->IvyResult<()>{
    let component=manifest.required("componentId")?.as_str()?;
    if !component.bytes().next().is_some_and(|c|c.is_ascii_lowercase())||!component.bytes().all(|c|c.is_ascii_lowercase()||c.is_ascii_digit()||c==b'-'){return Ok(())}
    let Some(shutdown)=manifest.get("shutdown").and_then(|value|value.get("timeoutMs")).and_then(IvyJson::as_f64).filter(|value|value.is_finite()&&value.fract()==0.0&&*value>=1.0&&*value<=9_007_199_254_740_991.0)else{return Ok(())};
    let main_command=manifest.get("entrypoint");let health_command=manifest.get("readiness").and_then(|value|value.get("command"));
    let main=command_path(main_command);let health=command_path(health_command);
    let main_source=main.as_ref().map(|path|shim(path,&format!("ivy-{component}"),shutdown,false));let health_source=health.as_ref().map(|path|shim(path,"ivy-health",shutdown,true));
    let native=main.as_ref().zip(main_source.as_ref()).is_some_and(|(path,source)|matches(&root.join(path),source))
        ||health.as_ref().zip(health_source.as_ref()).is_some_and(|(path,source)|matches(&root.join(path),source));
    if !native{return Ok(())}
    require(main_command.and_then(|value|value.get("executable"))==Some(&"node".into())&&health_command.and_then(|value|value.get("executable"))==Some(&"node".into())&&main!=health,"artifact_changed","Native dispatch commands differ from their retained v1 paths.")?;
    for(path,source)in [(main,main_source),(health,health_source)]{
        let(path,source)=path.zip(source).ok_or_else(||IvyError::new("artifact_changed","Native dispatch needs both expected v1 shims."))?;
        require(matches(&direct_file(root,&path)?,&source),"artifact_changed","Native dispatch shim differs from its exact v1 package.")?;
    }
    direct_file(root,"dist/native/dispatch.mjs")?;
    let platform=crate::portable_native_files::native_platform()?;
    let suffix=if cfg!(windows){".exe"}else{""};
    for name in [format!("ivy-{component}"),"ivy-health".into()]{direct_file(root,&format!("dist/native/{platform}/{name}{suffix}"))?;}
    let stamp=direct_file(root,&format!("dist/native/{platform}/build-info.json"))?;let info=read_json(&stamp,CONFIGURATION_BYTES).map_err(FileError::error)?;
    require(info.get("schemaVersion")==Some(&IvyJson::Number(1.0))&&info.get("buildId")==candidate.get("buildId")&&info.get("version")==manifest.get("version")&&info.get("componentId").is_none_or(|value|Some(value)==candidate.get("componentId")),"build_mismatch","Native target build differs from its prepared release identity.")?;
    if cfg!(windows){for selected in crate::portable_native_files::portable_native_files(&component){direct_file(root,selected)?;}}
    Ok(())
}
