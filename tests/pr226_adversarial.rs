//! Reviewer-only synthetic fault injection; no production implementation changes.
use private_ai_gateway::aggregator::{session::{AttestedSession, SessionDocument, SessionClaims, EvidenceRef}, session_cas::{pack, unpack, Packed}, session_store::{JsonlSessionStore, SessionStore}};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use serde_json::json;
use std::{fs, path::PathBuf, sync::atomic::{AtomicU64, Ordering}};
static N: AtomicU64 = AtomicU64::new(0);
struct Scratch(PathBuf);
impl Scratch { fn new() -> Self { let p=std::env::temp_dir().join(format!("pr226-reviewer-{}-{}",std::process::id(),N.fetch_add(1,Ordering::Relaxed))); fs::create_dir_all(&p).unwrap(); Self(p) } fn log(&self)->PathBuf {self.0.join("sessions.jsonl")} }
impl Drop for Scratch {fn drop(&mut self){let _=fs::remove_dir_all(&self.0);}}
fn fixture()->AttestedSession { AttestedSession::from_bytes(fs::read(concat!(env!("CARGO_MANIFEST_DIR"),"/tests/fixtures/sessions/phala-direct-a.json")).unwrap()).unwrap() }
fn small()->AttestedSession { AttestedSession::seal(SessionDocument{api_version:"aci/1".into(),upstream_name:"real-upstream".into(),endpoint:None,verifier_id:"test".into(),established_at:100,expires_at:200,identity:None,channel_binding:vec![],claims:SessionClaims::default(),evidence:EvidenceRef::default()}).unwrap() }

// Invariant: unfamiliar valid producer JSON must fall back rather than panic.
#[test] fn marker_collision_never_panics() { for s in [r#"{"$r":""}"#,r#"{"$r":"x"}"#,r#"{"$r":"€"}"#] { let outcome=std::panic::catch_unwind(||pack(s.as_bytes())); assert!(outcome.is_ok(),"valid JSON panics: {s}"); assert!(matches!(outcome.unwrap(),Packed::Whole(_))); } }

// Migration safety: a duplicate put after legacy adoption retains exact bytes.
#[test] fn legacy_reput_preserves_document() { let t=Scratch::new(); let p=t.log(); let s=fixture(); let rec=json!({"seq":0,"ts":100,"type":"session","fingerprint":"fp","retention_until":9000,"payload_b64":BASE64.encode(s.bytes())}); fs::write(&p,format!("{rec}\n")).unwrap(); let st=JsonlSessionStore::open(&p,100).unwrap(); assert!(st.get_session(s.session_id(),100).is_some()); st.put_session("fp",s.clone(),9000,100).unwrap(); assert_eq!(st.get_session(s.session_id(),100).map(|v|v.bytes().to_vec()),Some(s.bytes().to_vec())); }

// Regression candidate: simulate the residue of an interrupted create_new/write_all.
#[test] fn partial_chunk_cannot_be_acknowledged_as_success() { let t=Scratch::new();let p=t.log();let st=JsonlSessionStore::open(&p,100).unwrap(); let s=fixture();let Packed::Cas{chunks,..}=pack(s.bytes()) else {panic!()}; fs::write(p.with_extension("chunks").join(&chunks[0].0),b"partial").unwrap(); let result=st.put_session("fp",s.clone(),9000,100); if result.is_ok(){assert_eq!(st.get_session(s.session_id(),100).map(|v|v.bytes().to_vec()),Some(s.bytes().to_vec()),"acknowledged an unreadable session");} }

// Invariant: malformed replay ids cannot authorize deletion outside docs/.
#[test] fn replay_id_cannot_escape_gc_directory() { let t=Scratch::new();let p=t.log();let victim=t.0.join("victim");fs::write(&victim,b"must survive").unwrap(); let rec=json!({"seq":0,"ts":100,"type":"session2","fingerprint":"fp","id":"../victim","upstream_name":"x","expires_at":200,"retention_until":200,"whole":true});fs::write(&p,format!("{rec}\n")).unwrap(); let st=JsonlSessionStore::open(&p,100).unwrap();st.compact(300).unwrap();assert!(victim.exists(),"GC deleted unrelated sibling file"); }

// Invariant: unhashed index expiry is not authoritative over sealed expiry.
#[test] fn replay_metadata_cannot_revive_expired_session() { let t=Scratch::new();let p=t.log();let s=small();{let st=JsonlSessionStore::open(&p,100).unwrap();st.put_session("fp",s.clone(),9000,100).unwrap();}let mut rec:serde_json::Value=serde_json::from_slice(&fs::read(&p).unwrap()).unwrap();rec["expires_at"]=json!(9000);fs::write(&p,format!("{rec}\n")).unwrap();let st=JsonlSessionStore::open(&p,300).unwrap();assert!(st.current_session("fp",9000,300).is_none(),"returned session whose sealed validity already lapsed"); assert!(st.list_sessions(None,300).is_empty()); }

// Invariant: transient failure walking a live skeleton must abort sweeping its chunks.
#[test] fn failed_gc_mark_does_not_destroy_live_chunks() {let t=Scratch::new();let p=t.log();let s=fixture();let st=JsonlSessionStore::open(&p,100).unwrap();st.put_session("fp",s.clone(),9000,100).unwrap();let doc=p.with_extension("docs").join(s.session_id());let saved=fs::read(&doc).unwrap();fs::remove_file(&doc).unwrap();fs::create_dir(&doc).unwrap();st.compact(200).unwrap();fs::remove_dir(&doc).unwrap();fs::write(&doc,saved).unwrap();assert!(st.get_session(s.session_id(),200).is_some(),"mark read failure destroyed referenced chunks");}

// Invariant: opaque bytes and unknown serialization variants survive roundtrip.
#[test] fn adversarial_roundtrip_matrix() {let samples=[json!({"a":"UPPERCASE".repeat(300)}),json!({"data":format!("data:x;base64,{}",BASE64.encode(vec![255;2000]))}),json!({"embedded":format!("{{ \"float\": 1.50, \"padding\": \"{}\" }}","x".repeat(1100))}),json!({"$j":"ordinary provider field"}),json!({"a":"😀→\\\n\u{0}".repeat(300)})];for v in samples{let b=serde_json::to_vec(&v).unwrap();match pack(&b){Packed::Whole(w)=>assert_eq!(w,b),Packed::Cas{skeleton,chunks}=>assert_eq!(unpack(&skeleton,false,&mut |d|chunks.iter().find(|(k,_)|k==d).map(|(_,v)|v.clone())).unwrap(),b)}}}
