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
