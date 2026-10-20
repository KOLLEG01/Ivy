use crate::{IvyError,IvyJson,IvyResult,JsString,json::js_string};

fn require(condition:bool,message:&str)->IvyResult<()> {if condition{Ok(())}else{Err(IvyError::new("invalid_frame",message))}}
fn object(value:&IvyJson,message:&str)->IvyResult<()>{require(value.as_object().is_some(),message)}
pub fn text(value:Option<&IvyJson>,maximum:usize)->bool {value.and_then(IvyJson::as_js_string).is_some_and(|s|!s.0.is_empty()&&s.0.len()<=maximum)}
pub fn exact_keys(value:&IvyJson,allowed:&[&str])->bool {value.as_object().is_some_and(|p|p.iter().all(|(k,_)|allowed.iter().any(|a|k==&JsString::from(*a))))}
pub fn safe_integer(value:Option<&IvyJson>,minimum:f64)->bool {value.and_then(IvyJson::as_f64).is_some_and(|n|n.is_finite()&&n.fract()==0.0&&n>=minimum&&n.abs()<=9_007_199_254_740_991.0)}
pub fn equal(value:Option<&IvyJson>,literal:&str)->bool {value==Some(&IvyJson::from(literal))}
pub fn hash(value:Option<&IvyJson>)->bool {value.and_then(IvyJson::as_js_string).and_then(|s|s.to_utf8().ok()).is_some_and(|s|s.len()==71&&s.starts_with("sha256:")&&s[7..].bytes().all(|b|b.is_ascii_digit()||(b'a'..=b'f').contains(&b)))}

pub fn validate_request(value:&IvyJson)->IvyResult<()> {
    object(value,"Expected one JSON-RPC request envelope.")?;
    require(equal(value.get("jsonrpc"),"2.0")&&exact_keys(value,&["jsonrpc","id","method","params"]),"Expected one exact JSON-RPC 2.0 request envelope.")?;
    require(text(value.get("id"),256)||safe_integer(value.get("id"),0.0),"Request ID is outside its bounded JSON-RPC domain.")?;
    require(text(value.get("method"),192),"Request method is outside its bounded JSON-RPC domain.")?;
    require(value.get("params").and_then(IvyJson::as_object).is_some(),"Request params must be an object.")
}
pub fn validate_response_frame(value:&IvyJson)->IvyResult<()> {
    object(value,"Expected one provider response envelope.")?;
    require(equal(value.get("jsonrpc"),"2.0")&&text(value.get("id"),256),"Provider response correlation is invalid.")?;
    let result=value.get("result").is_some();let error=value.get("error").is_some();
    require(result!=error,"Provider response requires exactly one result or error.")?;
    require(exact_keys(value,if result{&["jsonrpc","id","result"]}else{&["jsonrpc","id","error"]}),"Provider response envelope contains unexpected fields.")?;
    if let Some(error)=value.get("error"){
        object(error,"Provider error must be an object.")?;
        require(exact_keys(error,&["code","message","data"])&&safe_integer(error.get("code"),-9_007_199_254_740_991.0)&&text(error.get("message"),2048)&&error.get("data").and_then(IvyJson::as_object).is_some(),"Provider error envelope is incomplete.")?;
        let data=error.get("data").unwrap();
        require(exact_keys(data,&["code","outcome","details"])&&text(data.get("code"),128)&&data.get("outcome").is_some_and(|v|["not_executed","unknown","completed"].iter().any(|s|js_string(v)==JsString::from(*s))),"Provider error outcome is invalid.")?;
    }Ok(())
}
pub fn validate_provider_call_frame(value:&IvyJson)->IvyResult<()> {
    object(value,"Expected one provider request envelope.")?;
    require(equal(value.get("jsonrpc"),"2.0")&&equal(value.get("method"),"provider.invoke")&&text(value.get("id"),256)&&exact_keys(value,&["jsonrpc","id","method","params"]),"Provider request correlation is invalid.")?;
    let params=value.get("params").ok_or_else(||IvyError::new("invalid_frame","Provider request params must be an object."))?;
    object(params,"Provider request params must be an object.")?;
    require(exact_keys(params,&["qualifiedName","arguments","definitionHash","generation","callerPrincipalId","operationId"])&&text(params.get("qualifiedName"),256)&&params.get("arguments").is_some()&&hash(params.get("definitionHash"))&&safe_integer(params.get("generation"),1.0)&&text(params.get("callerPrincipalId"),256)&&(params.get("operationId").is_none()||text(params.get("operationId"),256)),"Provider request routing fields are invalid.")
}

fn service_name(value:Option<&IvyJson>)->bool {
    value.and_then(IvyJson::as_js_string).is_some_and(|s| !s.0.is_empty() && s.0.len()<=64 && (97..=122).contains(&s.0[0]) && s.0.iter().all(|c| (97..=122).contains(c)||(48..=57).contains(c)||*c==45))
}

/// Validate correlation and selectors before the selected tool's payload schema.
pub fn validate_tool_routing(value: &IvyJson, discovery: bool) -> IvyResult<()> {
    object(value, "Tool call params must be an object.")?;
    let allowed: &[&str] = if discovery {
        &["qualifiedName", "arguments", "expectedDefinitionHash", "serviceNodeId", "operationId", "expectedCallerPrincipalId", "serviceName"]
    } else {
        &["qualifiedName", "arguments", "expectedDefinitionHash", "serviceNodeId", "operationId", "expectedCallerPrincipalId", "hostId", "serviceName", "resourceRef"]
    };
    let optional_text = |field: Option<&IvyJson>| field.is_none() || text(field, 256);
    if !(exact_keys(value, allowed)
        && text(value.get("qualifiedName"), 256)
        && value.get("arguments").is_some()
        && hash(value.get("expectedDefinitionHash"))
        && optional_text(value.get("operationId"))
        && optional_text(value.get("expectedCallerPrincipalId"))
        && optional_text(value.get("serviceNodeId"))
        && (value.get("serviceName").is_none() || service_name(value.get("serviceName")))
        && optional_text(value.get("hostId")))
    {
        return Err(IvyError::new("invalid_arguments", "Tool routing fields are invalid."));
    }
    if discovery && !text(value.get("serviceName"), 256) {
        return Err(IvyError::new("invalid_arguments", "Discovery calls require a service name."));
    }
    if let Some(reference) = value.get("resourceRef") {
        object(reference, "Resource reference must be an object.")?;
        if !(exact_keys(reference, &["serviceNodeId", "namespace", "kind", "nativeId"])
            && text(reference.get("serviceNodeId"), 256)
            && text(reference.get("namespace"), 64)
            && text(reference.get("kind"), 64)
            && text(reference.get("nativeId"), 1024))
        {
            return Err(IvyError::new("invalid_arguments", "Resource routing fields are invalid."));
        }
    }
    Ok(())
}
fn contract_version(value:Option<&IvyJson>)->bool {
    value.and_then(IvyJson::as_js_string).is_some_and(|s| {
        let parts:Vec<_>=s.0.split(|c|*c==46).collect();
        parts.len()==3 && parts.iter().all(|p|!p.is_empty() && (p.len()==1||p[0]!=48) && p.iter().all(|c|(48..=57).contains(c)))
    })
}
pub fn validate_service_notification_frame(value:&IvyJson)->IvyResult<()> {
    object(value,"Expected one provider notification envelope.")?;
    require(equal(value.get("jsonrpc"),"2.0")&&equal(value.get("method"),"service.notification")&&exact_keys(value,&["jsonrpc","method","params"]),"Provider notification envelope is invalid.")?;
    let missing=IvyJson::Null;
    let params=value.get("params").unwrap_or(&missing);
    object(params,"Provider notification params must be an object.")?;
    require(exact_keys(params,&["namespace","name","version","payload"])&&service_name(params.get("namespace"))&&text(params.get("name"),192)&&contract_version(params.get("version"))&&params.get("payload").is_some(),"Provider notification routing fields are invalid.")
}
pub fn validate_provider_notification_frame(value:&IvyJson)->IvyResult<()> {
    object(value,"Expected one subscribed provider notification envelope.")?;
    require(equal(value.get("jsonrpc"),"2.0")&&equal(value.get("method"),"notifications.provider")&&exact_keys(value,&["jsonrpc","method","params"]),"Subscribed provider notification envelope is invalid.")?;
    let missing=IvyJson::Null;
    let params=value.get("params").unwrap_or(&missing);
    object(params,"Subscribed provider notification params must be an object.")?;
    require(exact_keys(params,&["namespace","name","version","payload","serviceNodeId","generation"])&&service_name(params.get("namespace"))&&text(params.get("name"),192)&&contract_version(params.get("version"))&&params.get("payload").is_some()&&text(params.get("serviceNodeId"),256)&&safe_integer(params.get("generation"),1.0),"Subscribed provider notification routing fields are invalid.")
}
pub fn validate_event_availability_frame(value:&IvyJson)->IvyResult<()> {
    object(value,"Expected one event availability envelope.")?;
    require(equal(value.get("jsonrpc"),"2.0")&&equal(value.get("method"),"events.available")&&exact_keys(value,&["jsonrpc","method","params"]),"Event availability envelope is invalid.")?;
    let missing=IvyJson::Null;
    let params=value.get("params").unwrap_or(&missing);
    object(params,"Event availability params must be an object.")?;
    require(exact_keys(params,&["throughSequence"])&&safe_integer(params.get("throughSequence"),1.0),"Event availability sequence is invalid.")
}
