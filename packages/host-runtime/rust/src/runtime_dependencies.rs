//! Exact npm v3 package locations for preparation and portable runtime payloads.
use crate::{IvyError,IvyJson,IvyResult};
use ivy_contracts::{JsString,json::object_entries};
use std::collections::BTreeSet;

fn require(ok:bool,code:&str,message:&str)->IvyResult<()>{if ok{Ok(())}else{Err(IvyError::new(code,message))}}
fn object(value:&IvyJson)->bool{value.as_object().is_some()}
fn truth(value:Option<&IvyJson>)->bool{value==Some(&IvyJson::Bool(true))}
fn module_prefix(value:&JsString)->bool{value.0.iter().copied().take(13).eq("node_modules/".encode_utf16())}
fn segment(value:&str)->bool{
    let allowed=|b:u8|b.is_ascii_alphanumeric()||matches!(b,b'_'|b'~'|b'-');
    !value.is_empty()&&allowed(value.as_bytes()[0])&&value.bytes().all(|b|allowed(b)||b==b'.')
}
fn package_name(value:&str)->bool{
    if let Some(scoped)=value.strip_prefix('@'){
        let Some((scope,name))=scoped.split_once('/')else{return false};segment(scope)&&segment(name)
    }else{segment(value)}
}
fn package_location(value:&str)->bool{
    let Some(mut value)=value.strip_prefix("node_modules/")else{return false};
    loop{
        if let Some((package,next))=value.split_once("/node_modules/"){
            if !package_name(package){return false}value=next;
        }else{return package_name(value)}
    }
}
fn metadata(lock:&IvyJson)->IvyResult<&IvyJson>{
    let packages=lock.get("packages");
    require(object(lock)&&lock.get("lockfileVersion")==Some(&IvyJson::Number(3.0))&&packages.is_some_and(object),
        "invalid_arguments","Runtime dependency packaging requires npm v3 package location metadata.")?;
    let packages=packages.unwrap();
    for(location,value)in object_entries(packages){
        require(object(value),"invalid_arguments","Lockfile package metadata must be an object.")?;
        for key in ["dev","devOptional","optional","link"]{
            require(value.get(key).is_none_or(|v|matches!(v,IvyJson::Bool(_))),"invalid_arguments","Lockfile dependency classification must use booleans.")?;
        }
        if module_prefix(location){
            require(location.to_utf8().ok().is_some_and(|location|package_location(&location)),"invalid_arguments","Dependency location is not an exact contained npm package path.")?;
        }
    }
    Ok(packages)
}
pub fn development_package_locations(lock:&IvyJson)->IvyResult<Vec<JsString>>{
    let packages=metadata(lock)?;let mut excluded=Vec::new();
    for(location,value)in object_entries(packages){
        if !truth(value.get("dev"))||!module_prefix(location){continue}
        require(!truth(value.get("devOptional")),"invalid_arguments","A package cannot be both exclusively developmental and optionally required at runtime.")?;
        excluded.push(location.clone());
    }
    Ok(excluded)
}
fn workspace_target(value:&JsString)->bool{
    let u=&value.0;
    !u.is_empty()&&u.len()<=2048&&u[0]!=47&&!(u.len()>=3&&(u[0] as u8).is_ascii_alphabetic()&&u[0]<=127&&u[1]==58&&matches!(u[2],47|92))
        &&u.split(|c|matches!(*c,47|92)).all(|p|!p.is_empty()&&p!=[46,46])
}
/// Preserve Object.entries/Map insertion order, including lossless UTF-16 targets.
pub fn workspace_package_locations(lock:&IvyJson)->IvyResult<Vec<(JsString,JsString)>>{
    if object(lock)&&lock.get("lockfileVersion")==Some(&IvyJson::Number(3.0))&&lock.get("packages").is_none(){return Ok(Vec::new())}
    let packages=metadata(lock)?;let mut links=Vec::new();
    for(location,value)in object_entries(packages){
        if !truth(value.get("link")){continue}
        let target=value.get("resolved").and_then(IvyJson::as_js_string);
        require(location.to_utf8().ok().is_some_and(|p|package_location(&p))&&target.is_some_and(workspace_target),
            "invalid_arguments","Workspace link must identify one exact package and a contained source directory.")?;
        let target=JsString(target.unwrap().0.iter().map(|c|if *c==92{47}else{*c}).collect());
        links.push((location.clone(),target));
    }
    Ok(links)
}
fn resolved(packages:&IvyJson,owner:&str,name:&JsString)->Option<JsString>{
    let name=name.to_utf8().ok()?;let mut current=owner;
    loop{
        let candidate=if current.is_empty(){format!("node_modules/{name}")}else{format!("{current}/node_modules/{name}")};
        if packages.get(&candidate).is_some(){return Some(candidate.into())}
        if current.is_empty(){return None}
        current=current.rsplit_once("/node_modules/").map_or("",|(parent,_)|parent);
    }
}
/// Breadth-first ordered closure: required, optional and peer locations retain
/// their original order. Callers can use a set separately for membership checks.
pub fn runtime_package_locations(lock:&IvyJson,names:impl IntoIterator<Item=JsString>)->IvyResult<Vec<JsString>>{
    let mut requested=Vec::new();let mut known=BTreeSet::new();
    for name in names{if known.insert(name.clone()){requested.push(name)}}
    if requested.is_empty(){return Ok(Vec::new())}
    let packages=metadata(lock)?;let mut selected=Vec::new();let mut seen=BTreeSet::new();
    for name in requested{
        require(name.to_utf8().ok().is_some_and(|name|package_name(&name)),"invalid_arguments","Runtime package name is invalid.")?;
        let location=format!("node_modules/{}",name.to_utf8().unwrap());
        require(packages.get(&location).is_some_and(|m|!truth(m.get("link"))),"artifact_missing","Emitted runtime dependency is absent from the exact lock.")?;
        let location=JsString::from(location);if seen.insert(location.clone()){selected.push(location)}
    }
    let mut index=0;
    while index<selected.len(){
        let owner=selected[index].to_utf8().unwrap();index+=1;
        let value=packages.get(&owner).unwrap();
        for(field,required)in [("dependencies",true),("optionalDependencies",false),("peerDependencies",false)]{
            let dependencies=value.get(field);
            require(dependencies.is_none_or(object),"invalid_arguments","Runtime dependency metadata must be an object.")?;
            let Some(dependencies)=dependencies else{continue};
            for(name,_)in object_entries(dependencies){
                let location=resolved(packages,&owner,name);
                require(location.is_some()||!required,"artifact_missing","Locked runtime dependency is unavailable.")?;
                if let Some(location)=location{if seen.insert(location.clone()){selected.push(location)}}
            }
        }
    }
    Ok(selected)
}
