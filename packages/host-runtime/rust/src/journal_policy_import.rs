//! Conversion-only journal operations. Kept out of the normal lookup path.
use super::*;
use crate::policy_import::{PolicyImportRequest,ReviewedPolicyBinding};
use sha2::{Digest,Sha256};

type Targets=BTreeMap<String,(IvyJson,Vec<IvyJson>)>;
fn add(targets:&mut Targets,target:IvyJson,origin:IvyJson)->IvyResult<()>{
    validate_host("RuntimeTarget",&target)?;let key=policy_key(&target)?;
    if let Some((prior,origins))=targets.get_mut(&key){require(encoded(prior)?==encoded(&target)?,"target_conflict","Retained targets disagree at the same policy identity.")?;origins.push(origin);}
    else{targets.insert(key,(target,vec![origin]));}
    require(targets.len()<=10000,"limit_exceeded","Target policy import exceeds its review limit.")
}
fn request_json(request:&PolicyImportRequest)->IvyJson{
    IvyJson::object([("schemaVersion",IvyJson::Number(1.0)),("importId",request.import_id.as_str().into()),("expectedStateHash",request.expected_state_hash.as_str().into()),("requireComplete",request.require_complete.into()),
        ("bindings",IvyJson::Array(request.bindings.iter().map(|binding|IvyJson::object([("target",binding.target.clone()),("configurationPath",crate::layout::path_json(&binding.configuration_path)),("configurationHash",binding.configuration_hash.as_str().into())])).collect()))])
}
impl HostJournal {
    pub(crate) async fn open_policy_import(config:IvyJson,read_only:bool)->IvyResult<Self>{
        let state=internal(tokio::task::spawn_blocking(move||{
            validate_host("HostConfig",&config)?;
            let root=crate::node_path::filesystem_path(config.required("runtimeRoot")?.as_js_string().ok_or_else(IvyError::internal)?);
            let database=root.join("deployments.sqlite");let metadata=internal(fs::symlink_metadata(&database))?;
            require(metadata.is_file()&&!metadata.file_type().is_symlink(),"configuration_changed","Policy import needs an existing directly owned journal.")?;
            let db=internal(sqlite::open(&database,sqlite::OpenOptions{busy_timeout:Duration::from_millis(5000),read_only}))?;
            let version:i64=internal(db.query_row("PRAGMA user_version",[],|row|row.get(0)))?;
            require(version==1,"unsupported_storage","Convert the journal format before importing target policies.")?;
            let state=State{db,config,clock:Arc::new(SystemClock),history_limits:HistoryLimits::default()};
            require(state.text("SELECT value FROM meta WHERE key=?","hostId")?.as_ref()==Some(&state.config.required("hostId")?.as_str()?)
                &&state.text("SELECT value FROM meta WHERE key=?","installation")?.as_ref()==Some(&installation_record(&state.config)?),"target_conflict","Policy import must use the journal's existing installation identity.")?;
            Ok::<_,IvyError>(state)
        }).await)??;
        Ok(Self{state:Arc::new(Mutex::new(Some(state)))})
    }
    pub(crate) async fn policy_import_plan(&self)->IvyResult<IvyJson>{
        self.with(|state|{
            internal(state.db.execute_batch("BEGIN"))?;
            let result=state.import_plan();
            let release=internal(state.db.execute_batch(if result.is_ok(){"COMMIT"}else{"ROLLBACK"}));release?;result
        }).await
    }
    pub(crate) async fn apply_policy_import(&self,request:PolicyImportRequest)->IvyResult<IvyJson>{self.with(move|state|state.import_policies(request)).await}
}
impl State {
    fn import_targets(&self)->IvyResult<Targets>{
        let mut targets=Targets::new();
        let mut statement=internal(self.db.prepare("SELECT instance_id,value FROM runtime_targets ORDER BY instance_id"))?;
        for row in internal(statement.query_map([],|row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?))))?{
            let(id,text)=internal(row)?;let target=decoded(&text)?;
            require(target.get("instanceId")==Some(&id.as_str().into()),"target_conflict","Retained target key does not identify its instance.")?;
            add(&mut targets,target,IvyJson::object([("kind","current".into()),("id",id.into())]))?;
        }
        let mut statement=internal(self.db.prepare("SELECT operation_id,entry_json FROM operations ORDER BY operation_id"))?;
        for row in internal(statement.query_map([],|row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?))))?{
            let(id,text)=internal(row)?;let entry=decoded(&text)?;
            if let Some(target)=entry.get("activation").and_then(|value|value.get("previousTarget")).filter(|target|!target.is_null()){
                add(&mut targets,target.clone(),IvyJson::object([("kind","retained_rollback".into()),("id",id.into())]))?;
            }
        }
        let mut statement=internal(self.db.prepare("SELECT key,value FROM meta WHERE key GLOB 'runtime-policy:*' ORDER BY key"))?;
        for row in internal(statement.query_map([],|row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?))))?{
            let(key,text)=internal(row)?;let value=decoded(&text)?;let target=value.required("target")?.clone();
            require(policy_key(&target)?==key,"target_conflict","Retained policy key does not identify its whole target.")?;
            self.policy(&target)?.ok_or_else(IvyError::internal)?;
            add(&mut targets,target,IvyJson::object([("kind","associated".into()),("id",key.into())]))?;
        }
        Ok(targets)
    }
    fn import_state_hash(&self)->IvyResult<String>{
        let mut hash=Sha256::new();let mut bytes=0usize;
        let mut part=|value:&[u8]|->IvyResult<()>{bytes=bytes.saturating_add(value.len());require(bytes<=128*1024*1024,"limit_exceeded","Target policy import exceeds its review limit.")?;hash.update((value.len() as u64).to_le_bytes());hash.update(value);Ok(())};
        part(b"runtime-target-policy-import-v1")?;part(encoded(&self.config)?.as_bytes())?;
        for query in ["SELECT instance_id,value FROM runtime_targets ORDER BY instance_id","SELECT operation_id,entry_json FROM operations ORDER BY operation_id","SELECT candidate_id,candidate_json FROM candidates ORDER BY candidate_id","SELECT key,value FROM meta WHERE key NOT GLOB 'runtime-policy-import:*' ORDER BY key"]{
            part(query.as_bytes())?;let mut statement=internal(self.db.prepare(query))?;
            for row in internal(statement.query_map([],|row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?))))?{let(key,value)=internal(row)?;part(key.as_bytes())?;part(value.as_bytes())?;}
        }
        drop(part);Ok(format!("sha256:{:x}",hash.finalize()))
    }
    fn import_plan(&self)->IvyResult<IvyJson>{
        let mut entries=Vec::new();let mut missing=0;
        for(key,(target,origins))in self.import_targets()?{
            let prior=self.policy(&target)?;if prior.is_none(){missing+=1;}
            entries.push(IvyJson::object([("key",key.into()),("target",target),("origins",IvyJson::Array(origins)),("configurationPath",prior.as_ref().map_or(IvyJson::Null,|(path,_)|path.clone())),("configurationHash",prior.as_ref().map(|(_,config)|hash_json(config).map(IvyJson::from)).transpose()?.unwrap_or(IvyJson::Null))]));
        }
        Ok(IvyJson::object([("schemaVersion",IvyJson::Number(1.0)),("stateHash",self.import_state_hash()?.into()),("targets",IvyJson::Array(entries)),("unassociatedTargets",IvyJson::Number(missing as f64))]))
    }
    fn reviewed_policy(&self,binding:&ReviewedPolicyBinding)->IvyResult<IvyJson>{
        validate_host("RuntimeTarget",&binding.target)?;
        let accepted=read_accepted_configuration(&self.config,&binding.configuration_path)?;
        require(hash_json(&accepted)?==binding.configuration_hash,"configuration_changed","Reviewed policy bytes changed before import.")?;
        let target=&binding.target;let id=target.required("instanceId")?;
        let instance=accepted.required("instances")?.as_array().ok_or_else(IvyError::internal)?.iter().find(|instance|instance.get("instanceId")==Some(id)).ok_or_else(||IvyError::new("target_conflict","Reviewed policy does not contain its target instance."))?;
        let identity=encoded(&project(instance,&["componentId","serviceNodeId"]))?;
        require(self.text("SELECT value FROM meta WHERE key=?",&format!("instanceIdentity:{}",id.as_str()?))?.is_none_or(|prior|prior==identity),"target_conflict","Reviewed policy changes the instance's retained identity.")?;
        let candidate=self.candidate(&target.required("candidateId")?.as_str()?)?;
        require(candidate.get("componentId")==instance.get("componentId"),"target_conflict","Reviewed target and policy identify different components.")?;
        require(internal(crate::paths::inside(&path(accepted.required("runtimeRoot")?)?,&path(target.required("configPath")?)?))?,"target_conflict","Reviewed target configuration leaves its host runtime root.")?;
        Ok(accepted)
    }
    fn import_policies(&self,request:PolicyImportRequest)->IvyResult<IvyJson>{
        require(!request.import_id.is_empty()&&request.import_id.len()<=128&&request.import_id.bytes().all(|c|c.is_ascii_alphanumeric()||b"-_.".contains(&c)),"invalid_arguments","Policy import needs a bounded explicit import identity.")?;
        require(request.bindings.len()<=1024,"limit_exceeded","One target policy import batch exceeds its review limit.")?;
        let request_hash=hash_json(&request_json(&request))?;let receipt_key=format!("runtime-policy-import:{}",request.import_id);
        self.transaction(|state|{
            if let Some(prior)=state.value("SELECT value FROM meta WHERE key=?",&receipt_key)?{
                require(prior.get("requestHash")==Some(&request_hash.as_str().into()),"mutation_conflict","Policy import identity already has different reviewed arguments.")?;
                for binding in &request.bindings{state.reviewed_policy(binding)?;let Some((reference,_))=state.policy(&binding.target)?else{return Err(IvyError::new("configuration_changed","Completed import lost a target association."))};require(reference==crate::layout::path_json(&binding.configuration_path),"target_conflict","Completed import association was rebound.")?;}
                return Ok(prior.required("receipt")?.clone());
            }
            let before=state.import_state_hash()?;require(before==request.expected_state_hash,"revision_conflict","Reviewed journal state changed before policy import.")?;
            let targets=state.import_targets()?;let mut seen=BTreeSet::new();let mut mappings=Vec::new();
            for binding in &request.bindings{
                let key=policy_key(&binding.target)?;require(seen.insert(key.clone()),"invalid_arguments","Policy import repeats a target identity.")?;
                let Some((retained,_))=targets.get(&key)else{return Err(IvyError::new("target_conflict","Reviewed target is not retained by this journal."))};
                require(encoded(retained)?==encoded(&binding.target)?,"target_conflict","Reviewed import must keep the whole exact retained target.")?;
                state.reviewed_policy(binding)?;let prior=state.policy(&binding.target)?;
                state.associate_policy(&binding.target,Some(crate::layout::path_json(&binding.configuration_path)))?;
                mappings.push(IvyJson::object([("key",key.into()),("target",binding.target.clone()),("configurationPath",crate::layout::path_json(&binding.configuration_path)),("configurationHash",binding.configuration_hash.as_str().into()),("action",if prior.is_some(){"retained"}else{"imported"}.into())]));
            }
            let remaining=targets.into_iter().filter_map(|(key,(target,_))|match state.policy(&target){Ok(Some(_))=>None,Ok(None)=>Some(Ok(IvyJson::from(key))),Err(error)=>Some(Err(error))}).collect::<IvyResult<Vec<_>>>()?;
            require(!request.require_complete||remaining.is_empty(),"configuration_changed","Policy import still has unreviewed targets; no batch changes were committed.")?;
            let receipt=IvyJson::object([("schemaVersion",IvyJson::Number(1.0)),("importId",request.import_id.as_str().into()),("beforeStateHash",before.into()),("afterStateHash",state.import_state_hash()?.into()),("mappings",IvyJson::Array(mappings)),("remainingTargets",IvyJson::Array(remaining))]);
            state.save_meta(&receipt_key,&encoded(&IvyJson::object([("requestHash",request_hash.clone().into()),("receipt",receipt.clone())]))?)?;Ok(receipt)
        })
    }
}
