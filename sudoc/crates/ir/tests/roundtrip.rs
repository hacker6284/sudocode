//! Wire-format round-trip for hand-constructed IR (no sudoc-types dep — cycle).

use sudoc_ir::{
    wire, BinaryOp, BoundaryTy, Builtin, IrExpr, IrExprKind, IrFunc, IrMatchArm, IrModule,
    IrParam, IrPattern, IrStmt, Place, Ty, UnaryOp,
};

fn lit_int(n: i64) -> IrExpr {
    IrExpr {
        ty: Ty::Int,
        kind: IrExprKind::Int(n),
    }
}

fn lit_float(f: f64) -> IrExpr {
    IrExpr {
        ty: Ty::Float,
        kind: IrExprKind::Float(f),
    }
}

fn sample_module() -> IrModule {
    IrModule {
        name: "sample".into(),
        imports: vec![],
        records: vec![],
        enums: vec![],
        consts: vec![],
        funcs: vec![IrFunc {
            name: "mix".into(),
            export: true,
            params: vec![IrParam {
                name: "s".into(),
                inout: false,
                ty: Ty::list(Ty::Int),
                boundary: BoundaryTy::Text,
            }],
            ret: Some(Ty::Int),
            ret_boundary: Some(BoundaryTy::Int),
            body: vec![
                IrStmt::Assign {
                    target: Place::Var("n".into()),
                    value: lit_int(-42),
                    declares: true,
                },
                IrStmt::Assign {
                    target: Place::Var("t".into()),
                    value: IrExpr {
                        ty: Ty::list(Ty::Int),
                        kind: IrExprKind::Text(vec![104, 101, 108, 108, 111]),
                    },
                    declares: true,
                },
                IrStmt::Assign {
                    target: Place::Var("f".into()),
                    value: lit_float(1.5),
                    declares: true,
                },
                IrStmt::If {
                    arms: vec![(
                        IrExpr {
                            ty: Ty::Bool,
                            kind: IrExprKind::Binary {
                                op: BinaryOp::Lt,
                                lhs: Box::new(IrExpr {
                                    ty: Ty::Int,
                                    kind: IrExprKind::Local("n".into()),
                                }),
                                rhs: Box::new(lit_int(0)),
                            },
                        },
                        vec![IrStmt::Skip],
                    )],
                    else_block: Some(vec![IrStmt::Break]),
                },
                IrStmt::While {
                    cond: IrExpr {
                        ty: Ty::Bool,
                        kind: IrExprKind::Bool(true),
                    },
                    body: vec![IrStmt::Continue],
                },
                IrStmt::Match {
                    scrutinee: lit_int(1),
                    arms: vec![
                        IrMatchArm {
                            pattern: IrPattern::Int(1),
                            body: vec![IrStmt::Skip],
                        },
                        IrMatchArm {
                            pattern: IrPattern::Wildcard,
                            body: vec![IrStmt::Skip],
                        },
                    ],
                },
                IrStmt::Expr(IrExpr {
                    ty: Ty::Int,
                    kind: IrExprKind::Builtin {
                        builtin: Builtin::AbsInt,
                        args: vec![IrExpr {
                            ty: Ty::Int,
                            kind: IrExprKind::Unary {
                                op: UnaryOp::Neg,
                                operand: Box::new(IrExpr {
                                    ty: Ty::Int,
                                    kind: IrExprKind::Local("n".into()),
                                }),
                            },
                        }],
                    },
                }),
                IrStmt::Return(Some(IrExpr {
                    ty: Ty::Int,
                    kind: IrExprKind::Local("n".into()),
                })),
            ],
        }],
        tests: vec![],
    }
}

#[test]
fn wire_roundtrip_equality() {
    let modules = vec![sample_module()];
    let json = wire::to_wire_json(&modules).expect("serialize");
    // i64 leaves are decimal strings; text scalars stay plain numbers.
    assert!(json.contains("\"Int\":\"-42\"") || json.contains("\"Int\": \"-42\""));
    assert!(json.contains("[104,101,108,108,111]") || json.contains("[104, 101, 108, 108, 111]"));
    let decoded = wire::from_wire_json(&json).expect("deserialize");
    assert_eq!(modules, decoded);
}

#[test]
fn wire_rejects_infer() {
    let modules = vec![IrModule {
        name: "bad".into(),
        imports: vec![],
        records: vec![],
        enums: vec![],
        consts: vec![],
        funcs: vec![IrFunc {
            name: "f".into(),
            export: false,
            params: vec![],
            ret: Some(Ty::Infer(0)),
            ret_boundary: None,
            body: vec![IrStmt::Return(None)],
        }],
        tests: vec![],
    }];
    let err = wire::to_wire_json(&modules).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("Infer"),
        "expected Infer rejection, got: {msg}"
    );
}

#[test]
fn wire_nonfinite_floats() {
    let mut m = sample_module();
    m.funcs[0].body.insert(
        0,
        IrStmt::Assign {
            target: Place::Var("nan".into()),
            value: lit_float(f64::NAN),
            declares: true,
        },
    );
    m.funcs[0].body.insert(
        1,
        IrStmt::Assign {
            target: Place::Var("inf".into()),
            value: lit_float(f64::INFINITY),
            declares: true,
        },
    );
    m.funcs[0].body.insert(
        2,
        IrStmt::Assign {
            target: Place::Var("ninf".into()),
            value: lit_float(f64::NEG_INFINITY),
            declares: true,
        },
    );
    let json = wire::to_wire_json(&[m.clone()]).expect("serialize nonfinite");
    assert!(json.contains("\"nan\""));
    assert!(json.contains("\"inf\""));
    assert!(json.contains("\"-inf\""));
    let decoded = wire::from_wire_json(&json).expect("deserialize nonfinite");
    // NaN != NaN under PartialEq; check bits via is_nan / classification.
    let kinds: Vec<&IrExprKind> = decoded[0].funcs[0]
        .body
        .iter()
        .filter_map(|s| match s {
            IrStmt::Assign { value, .. } => Some(&value.kind),
            _ => None,
        })
        .collect();
    match kinds[0] {
        IrExprKind::Float(f) => assert!(f.is_nan()),
        other => panic!("expected NaN float, got {other:?}"),
    }
    match kinds[1] {
        IrExprKind::Float(f) => assert_eq!(*f, f64::INFINITY),
        other => panic!("expected +inf, got {other:?}"),
    }
    match kinds[2] {
        IrExprKind::Float(f) => assert_eq!(*f, f64::NEG_INFINITY),
        other => panic!("expected -inf, got {other:?}"),
    }
}
