// Copyright © 2016-2017 VMware, Inc. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0

#![cfg_attr(feature="cargo-clippy", allow(let_and_return))]
#![cfg_attr(feature="cargo-clippy", allow(needless_return))]

//! This module contains test functions for specific API operations. It's intended to be general
//! enough to use for multiple tests.

use super::scheduler::Scheduler;
use rabble::{self, Envelope};
use vertree::NodeType;
use haret::Msg;
use haret::vr::{VrState, VrMsg, VrCtx};
use haret::vr::{ClientOp, ClientRequest};
use haret::api::{ApiReq, ApiRsp, ApiError, TreeOp, TreeOpResult};
use vertree::{self, Reply, Value};

pub fn assert_create_response(scheduler: &Scheduler,
                              request: VrMsg,
                              reply: Envelope<Msg>) -> Result<(), String>
{
    let (request_num, api_req, api_rsp) = match_client_reply(request, reply)?;
    let (path, ty) = if let ApiReq::TreeOp(TreeOp::CreateNode {path, ty}) = api_req {
        (path, ty)
    } else {
        fail!()
    };

    match api_rsp {
        ApiRsp::Ok |
        ApiRsp::Error(ApiError::AlreadyExists(_)) => {
            assert_successful_create(scheduler, &path, request_num, ty)
        },
        ApiRsp::Error(ApiError::PathMustEndInDirectory(_)) => {
            Ok(())
        },
        e => {
            println!("e = {:?}", e);
            fail!()
        }
    }
}

/// Assertions for puts that aren't CAS
pub fn assert_put_response(scheduler: &Scheduler,
                           request: VrMsg,
                           reply: Envelope<Msg>) -> Result<(), String>
{
    let (request_num, api_req, api_rsp) = match_client_reply(request, reply)?;
    let (path, data) =
        if let ApiReq::TreeOp(TreeOp::BlobPut {path, val, ..}) = api_req {
            (path, val)
        } else {
            fail!()
        };

    match api_rsp {
        ApiRsp::TreeOpResult(TreeOpResult::Ok(_)) => {
            assert_successful_put_or_get(scheduler, path, request_num, &data)
        },
        ApiRsp::Error(ApiError::AlreadyExists(_)) |
        ApiRsp::Error(ApiError::PathMustEndInDirectory(_)) => Ok(()),
        ApiRsp::Error(ApiError::WrongType(_, ty)) => safe_assert_eq!(ty, NodeType::Directory),
        ApiRsp::Error(ApiError::DoesNotExist(_)) => {
            assert_element_not_found_primary(scheduler, path)
        },
        e => {
            println!("put unhandled error = {:?}", e);
            fail!()
        }
    }
}

pub fn assert_get_response(scheduler: &Scheduler,
                           request: VrMsg,
                           reply: Envelope<Msg>) -> Result<(), String>
{
    let (request_num, api_req, api_rsp) = match_client_reply(request, reply)?;
    let path = if let ApiReq::TreeOp(TreeOp::BlobGet {path, ..}) = api_req {
        path
    } else {
        fail!()
    };

    match api_rsp {
        ApiRsp::TreeOpResult(TreeOpResult::Blob(data, _)) => {
            assert_successful_put_or_get(scheduler, path, request_num, &data)
        },
        ApiRsp::Error(ApiError::AlreadyExists(_)) |
        ApiRsp::Error(ApiError::PathMustEndInDirectory(_)) => Ok(()),
        ApiRsp::Error(ApiError::WrongType(_, ty)) => safe_assert_eq!(ty, NodeType::Directory),
        ApiRsp::Error(ApiError::DoesNotExist(_)) => {
            assert_element_not_found_primary(scheduler, path)
        },
        e => {
            println!("get unhandled error = {:?}", e);
            fail!()
        }
    }
}

/// Attempt to retrieve a client reply and extract useful data from it, along with data from the
/// request.
fn match_client_reply(request: VrMsg, reply: Envelope<Msg>)
  -> Result<(u64, ApiReq, ApiRsp), String>
{
    if let VrMsg::ClientRequest(ClientRequest {op, request_num, ..})= request {
        if let rabble::Msg::User(Msg::Vr(VrMsg::ClientReply(reply))) = reply.msg {
            let _ = safe_assert_eq!(reply.request_num, request_num, op);
            return Ok((request_num, op, reply.value));
        }
    }
    fail!()
}


pub fn assert_successful_create(scheduler: &Scheduler,
                                path: &str,
                                request_num: u64,
                                ty: NodeType) -> Result<(), String>
{
    assert_majority_of_nodes_contain_op(scheduler, request_num)?;
    assert_primary_has_committed_op(scheduler, request_num)?;
    assert_path_exists_in_primary_backend(scheduler, path, ty)
}

pub fn assert_successful_put_or_get(scheduler: &Scheduler,
                                    path: String,
                                    request_num: u64,
                                    data: &[u8]) -> Result<(), String>
{
    assert_majority_of_nodes_contain_op(scheduler, request_num)?;
    assert_primary_has_committed_op(scheduler, request_num)?;
    assert_data_matches_primary_backend(scheduler, path, data)
}

pub fn assert_majority_of_nodes_contain_op(scheduler: &Scheduler,
                                           request_num: u64) -> Result<(), String> {
    let mut contained_in_log = 0;
    for r in &scheduler.new_config.replicas {
        if let Some(state) = scheduler.get_state(r) {
            if is_client_request_last_in_log(state.ctx(), request_num) {
                contained_in_log += 1;
            }
        }
    }
    safe_assert!(contained_in_log >= scheduler.quorum())
}

pub fn assert_primary_has_committed_op(scheduler: &Scheduler,
                                       request_num: u64) -> Result<(), String>
{
    if let Some(ref primary) = scheduler.primary {
        let state = scheduler.get_state(primary).unwrap();
        match state {
            VrState::Primary(_) => { }
            _ => { fail!(); }
        }
        let ctx = state.ctx();
        safe_assert_eq!(ctx.op, ctx.commit_num)?;
        safe_assert!(is_client_request_last_in_log(ctx, request_num))
    } else {
        fail!()
    }
}

fn assert_data_matches_primary_backend(scheduler: &Scheduler,
                                       path: String,
                                       data: &[u8]) -> Result<(), String>
{
    if let Some(ref primary) = scheduler.primary {
        let state = scheduler.get_state(primary).unwrap();
        let ctx = state.ctx();
        match ctx.backend.tree.blob_get(path) {
            Ok(Reply {value, ..}) => {
                if let Value::Blob(blob) = value {
                    return safe_assert_eq!(blob, data);
                }
                fail!()
            },
            _ => fail!()
        }
    } else {
        fail!()
    }
}

fn assert_path_exists_in_primary_backend(scheduler: &Scheduler,
                                         path: &str,
                                         ty: NodeType) -> Result<(), String>
{
    if let Some(ref primary) = scheduler.primary {
        let state = scheduler.get_state(primary).unwrap();
        let ctx = state.ctx();

        if ctx.backend.tree.find(path, ty.into()).is_err() {
            // Check to see if it was already created as a directory
            if ctx.backend.tree.find(path, vertree::NodeType::Directory).is_err() {
                fail!()
            }
        }
        Ok(())
    } else {
        fail!()
    }
}

fn assert_element_not_found_primary(scheduler: &Scheduler,
                                    path: String) -> Result<(), String>
{
    if let Some(ref primary) = scheduler.primary {
        let state = scheduler.get_state(primary).unwrap();
        let ctx = state.ctx();
        safe_assert!(ctx.backend.tree.blob_get(path).is_err())
    } else {
        fail!()
    }
}

fn is_client_request_last_in_log(ctx: &VrCtx, request_num: u64) -> bool {
    if ctx.op == 0 { return false; }
    let msg = &ctx.log[(ctx.op - 1) as usize];
    if let ClientOp::Request(ClientRequest {request_num: logged_num, ..}) = *msg {
        if request_num == logged_num {
            return true;
        }
    }
    false
}

