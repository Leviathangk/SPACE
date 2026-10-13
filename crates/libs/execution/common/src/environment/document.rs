//! Stored schema-v1 documents. Database migrations remain owned by the first
//! policy store; this codec does not reinterpret legacy execution JSON.
use super::*;

#[cfg(test)]
mod tests;

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    schema_version: u32,
    reference: EnvironmentReference,
    descriptor: EnvironmentSpec,
}

#[derive(Debug, thiserror::Error)]
pub enum EnvironmentDocumentError {
    #[error("invalid environment JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Definition(#[from] EnvironmentDecodeError),
    #[error("environment document contains unknown fields or noncanonical scalar values")]
    UnrecognizedRepresentation,
}

impl EnvironmentDefinition {
    pub fn to_document(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(&Document {
            schema_version: ENVIRONMENT_SCHEMA_VERSION,
            reference: self.reference,
            descriptor: self.descriptor.spec().clone(),
        })
    }

    pub fn from_document(text: &str) -> Result<Self, EnvironmentDocumentError> {
        // Decode directly first so duplicate keys are rejected, including inside
        // reused legacy types. Those types intentionally retain permissive JSON
        // readers; compare parsed trees here to reject any fields they discarded.
        let document: Document = serde_json::from_str(text)?;
        if document.schema_version != ENVIRONMENT_SCHEMA_VERSION {
            return Err(EnvironmentDecodeError::UnsupportedSchema(document.schema_version).into());
        }
        let supplied: serde_json::Value = serde_json::from_str(text)?;
        if supplied != serde_json::to_value(&document)? {
            return Err(EnvironmentDocumentError::UnrecognizedRepresentation);
        }
        let descriptor = EnvironmentDescriptor::try_from(document.descriptor)
            .map_err(EnvironmentDecodeError::from)?;
        Ok(Self::new(descriptor).check_reference(document.reference)?)
    }
}
