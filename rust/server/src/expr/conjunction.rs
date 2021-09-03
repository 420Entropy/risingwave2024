use std::convert::TryFrom;

use protobuf::Message;

use risingwave_proto::expr::{ExprNode, ExprNode_ExprNodeType, FunctionCall};

use crate::array::{ArrayRef, DataChunk};
use crate::error::ErrorCode::{InternalError, ProtobufError};
use crate::error::{Result, RwError};
use crate::expr::build_from_proto as expr_build_from_proto;
use crate::expr::BoxedExpression;
use crate::expr::Expression;
use crate::types::{build_from_proto as type_build_from_proto, DataType, DataTypeRef};
use crate::vector_op::conjunction::{vector_and, vector_not, vector_or};

pub enum ConjunctionOperatorKind {
    And,
    Or,
    Not,
}

pub(super) struct ConjunctionExpression {
    return_type: DataTypeRef,
    kind: ConjunctionOperatorKind,
    lhs: BoxedExpression,
    rhs: Option<BoxedExpression>,
}

impl Expression for ConjunctionExpression {
    fn return_type(&self) -> &dyn DataType {
        &*self.return_type
    }

    fn return_type_ref(&self) -> DataTypeRef {
        self.return_type.clone()
    }

    fn eval(&mut self, input: &DataChunk) -> Result<ArrayRef> {
        let lhs = self.lhs.eval(input)?;
        match self.kind {
            ConjunctionOperatorKind::And => {
                // the rhs can't be None
                let rhs = self.rhs.as_mut().unwrap().eval(input)?;
                vector_and(lhs, rhs)
            }
            ConjunctionOperatorKind::Or => {
                let rhs = self.rhs.as_mut().unwrap().eval(input)?;
                vector_or(lhs, rhs)
            }
            ConjunctionOperatorKind::Not => vector_not(lhs),
        }
    }
}

impl<'a> TryFrom<&'a ExprNode> for ConjunctionExpression {
    type Error = RwError;
    fn try_from(proto: &'a ExprNode) -> Result<Self> {
        let function_call_node =
            FunctionCall::parse_from_bytes(proto.get_body().get_value()).map_err(ProtobufError)?;
        let return_type = type_build_from_proto(proto.get_return_type())?;
        let lhs =
            expr_build_from_proto(function_call_node.get_children().get(0).ok_or_else(|| {
                InternalError("conjunction expression must have lhs".to_string())
            })?)?;
        match proto.get_expr_type() {
            ExprNode_ExprNodeType::AND => {
                let rhs =
                    expr_build_from_proto(function_call_node.get_children().get(1).ok_or_else(
                        || InternalError("AND expression must have rhs".to_string()),
                    )?)?;
                Ok(Self {
                    return_type,
                    kind: ConjunctionOperatorKind::And,
                    rhs: Some(rhs),
                    lhs,
                })
            }
            ExprNode_ExprNodeType::OR => {
                let rhs =
                    expr_build_from_proto(function_call_node.get_children().get(1).ok_or_else(
                        || InternalError("OR expression must have rhs".to_string()),
                    )?)?;
                Ok(Self {
                    return_type,
                    kind: ConjunctionOperatorKind::Or,
                    rhs: Some(rhs),
                    lhs,
                })
            }
            ExprNode_ExprNodeType::NOT => Ok(Self {
                return_type,
                kind: ConjunctionOperatorKind::Not,
                rhs: None,
                lhs,
            }),
            _ => Err(InternalError("unsupported conjunction operator ".to_string()).into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::array::{ArrayRef, BoolArray};
    use crate::error::Result;
    use crate::util::downcast_ref;
    use protobuf::well_known_types::Any as AnyProto;
    use protobuf::RepeatedField;
    use risingwave_proto::data::DataType as DataTypeProto;
    use risingwave_proto::expr::ExprNode_ExprNodeType::{AND, NOT, OR};
    use risingwave_proto::expr::InputRefExpr;

    #[test]
    fn test_execute() {
        mock_execute(
            &[Some(true), Some(false), None],
            &[Some(false), Some(false), Some(false)],
            AND,
            &[Some(false), Some(false), None],
        );
        mock_execute(
            &[Some(true), Some(false), None],
            &[Some(false), Some(false), Some(false)],
            OR,
            &[Some(true), Some(false), None],
        );
        mock_execute(
            &[Some(true), Some(false), None],
            &[Some(false), Some(false), Some(false)],
            NOT,
            &[Some(false), Some(true), None],
        );
    }

    fn mock_execute(
        lhs: &[Option<bool>],
        rhs: &[Option<bool>],
        kind: ExprNode_ExprNodeType,
        target: &[Option<bool>],
    ) {
        let lhs = create_boolarray(lhs).unwrap();
        let rhs = create_boolarray(rhs).unwrap();
        let data_chunk = DataChunk::builder()
            .cardinality(2)
            .arrays([lhs, rhs].to_vec())
            .build();
        let expr = create_cmp_expression(0, 1, kind).unwrap();
        let mut cmp_excutor = ConjunctionExpression::try_from(&expr).unwrap();
        let res = cmp_excutor.eval(&data_chunk).unwrap();
        let res = downcast_ref(res.as_ref()).unwrap() as &BoolArray;
        let iter = res.as_iter().unwrap();
        for (res, &tar) in iter.zip(target.into_iter()) {
            assert_eq!(res, tar);
        }
    }
    fn create_cmp_expression(
        idx1: i32,
        idx2: i32,
        kind: ExprNode_ExprNodeType,
    ) -> Result<ExprNode> {
        let mut expr = ExprNode::new();
        expr.set_expr_type(kind);
        let lhs = create_inputref(idx1)?;
        let rhs = create_inputref(idx2)?;
        let mut fc = FunctionCall::new();
        let fc_body = RepeatedField::from_slice(&[lhs, rhs]);
        fc.set_children(fc_body);
        expr.set_body(AnyProto::pack(&fc).unwrap());
        let mut boolen = DataTypeProto::new();
        boolen.set_type_name(risingwave_proto::data::DataType_TypeName::BOOLEAN);
        expr.set_return_type(boolen);
        let _t = expr.get_return_type();
        Ok(expr)
    }

    fn create_inputref(idx: i32) -> Result<ExprNode> {
        let mut expr = ExprNode::new();
        expr.set_expr_type(ExprNode_ExprNodeType::INPUT_REF);
        let mut body = InputRefExpr::new();
        body.set_column_idx(idx);
        expr.set_body(AnyProto::pack(&body).unwrap());
        let mut int32 = DataTypeProto::new();
        int32.set_type_name(risingwave_proto::data::DataType_TypeName::INT32);
        expr.set_return_type(int32);
        Ok(expr)
    }

    fn create_boolarray(vec: &[Option<bool>]) -> Result<ArrayRef> {
        BoolArray::from_values(vec)
    }
}
