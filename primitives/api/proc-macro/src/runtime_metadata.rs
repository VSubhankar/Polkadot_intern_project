// This file is part of Substrate.

// Copyright (C) 2018-2022 Parity Technologies (UK) Ltd.
// SPDX-License-Identifier: Apache-2.0

// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// 	http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use proc_macro2::TokenStream;
use quote::quote;
use syn::{parse_quote, ImplItemMethod, ItemImpl, ItemTrait, Path, Result};

use crate::{
	common::CHANGED_IN_ATTRIBUTE,
	utils::{
		extract_impl_trait, filter_cfg_attributes, generate_decl_docs_getter,
		generate_runtime_mod_name_for_trait, get_doc_literals, RequireQualifiedTraitPath,
	},
};

/// Generate documentation getters for `decl_runtime_api` macro.
///
/// The documentation is exposed for the runtime API metadata.
///
/// This method exposes the following functions for the latest trait version:
/// - `trait_[TRAIT_NAME]_decl_runtime_docs`: Extract trait documentation
/// - `[METHOD_NAME]_decl_runtime_docs`: One function for each method to extract its documentation
pub fn generate_decl_docs(decl: &ItemTrait, crate_: &TokenStream) -> TokenStream {
	if cfg!(feature = "no-metadata-docs") {
		return quote!()
	}

	let mut methods = Vec::new();

	for item in &decl.items {
		// Collect metadata for methods only.
		let syn::TraitItem::Method(method) = item else {
	                   continue
	    };

		let signature = &method.sig;
		let method_name = generate_decl_docs_getter(&signature.ident, false);

		let is_changed_in = method
			.attrs
			.iter()
			.find(|attr| attr.path.is_ident(CHANGED_IN_ATTRIBUTE))
			.is_some();
		// Report docs for the latest methods only.
		if is_changed_in {
			continue
		}

		let attrs = filter_cfg_attributes(&method.attrs);
		let docs = get_doc_literals(&method.attrs);
		methods.push(quote!(
				#( #attrs )*
				pub fn #method_name() -> #crate_::vec::Vec<&'static str> {
						#crate_::vec![ #( #docs, )* ]
				}
		));
	}

	let trait_name = generate_decl_docs_getter(&decl.ident, true);
	let docs = get_doc_literals(&decl.attrs);
	let attrs = filter_cfg_attributes(&decl.attrs);

	quote!(
			#( #attrs )*
			pub fn #trait_name() -> #crate_::vec::Vec<&'static str> {
					#crate_::vec![ #( #docs, )* ]
			}

			#( #methods )*
	)
}

/// Generate the runtime metadata for a given method.
fn generate_method_metadata(
	method: &ImplItemMethod,
	crate_: &TokenStream,
	hidden_mod: &Path,
) -> Result<TokenStream> {
	let signature = &method.sig;
	let mut inputs = Vec::new();

	for input in &signature.inputs {
		let syn::FnArg::Typed(typed) = input else {
				// Exclude `self` from metadata collection.
                       continue
               };

		let pat = &typed.pat;
		let name = format!("{}", quote!(#pat));
		let ty = &typed.ty;

		inputs.push(quote!(
				#crate_::metadata::v15::ParamMetadata {
						name: #name,
						ty: #crate_::scale_info::meta_type::<#ty>(),
				}
		));
	}

	let output = match &signature.output {
		syn::ReturnType::Default => quote!(#crate_::scale_info::meta_type::<()>()),
		syn::ReturnType::Type(_, ty) => quote!(#crate_::scale_info::meta_type::<#ty>()),
	};

	// String method name including quotes for constructing `v15::MethodMetadata`.
	let method_name = format!("{}", signature.ident);
	let docs = if cfg!(feature = "no-metadata-docs") {
		quote!(crate_::vec![])
	} else {
		// Function getter for the documentation.
		let doc_getter = generate_decl_docs_getter(&signature.ident, false);
		quote!(#hidden_mod::#doc_getter())
	};

	let attrs = filter_cfg_attributes(&method.attrs);

	Ok(quote!(
			#( #attrs )*
			#crate_::metadata::v15::MethodMetadata {
					name: #method_name,
					inputs: #crate_::vec![ #( #inputs, )* ],
					output: #output,
					docs: #docs,
			}
	))
}

/// Generate the runtime metadata for a given trait.
fn generate_trait_metadata(impl_: &ItemImpl, crate_: &TokenStream) -> Result<TokenStream> {
	let mut trait_ = extract_impl_trait(&impl_, RequireQualifiedTraitPath::Yes)?.clone();

	// Implementation traits are always references with a path `impl client::Core ...`
	// The trait name is the last segment of this path.
	let trait_name_ident = &trait_
		.segments
		.last()
		.as_ref()
		.expect("Trait path should always contain at least one item; qed")
		.ident;

	let trait_doc_getter = generate_decl_docs_getter(trait_name_ident, true);
	let trait_name = format!("{}", trait_name_ident);

	// Generate `runtime_decl_for_[TRAIT_NAME]` module that contains the documentation getters.
	let runtime_decl_mod = generate_runtime_mod_name_for_trait(trait_name_ident);

	// Get absolute path to the `runtime_decl_for_` module by replacing the last segment.
	if let Some(segment) = trait_.segments.last_mut() {
		*segment = parse_quote!(#runtime_decl_mod);
	}

	let docs = if cfg!(feature = "no-metadata-docs") {
		quote!(crate_::vec![])
	} else {
		quote!(#trait_::#trait_doc_getter())
	};

	let attrs = filter_cfg_attributes(&impl_.attrs);

	let mut methods = Vec::new();
	for item in &impl_.items {
		// Collect metadata for methods only.
		let syn::ImplItem::Method(method) = item else {
                       continue;
        };

		let metadata = generate_method_metadata(&method, crate_, &trait_)?;
		methods.push(metadata);
	}

	Ok(quote!(
			#( #attrs )*
			#crate_::metadata::v15::TraitMetadata {
					name: #trait_name,
					methods: #crate_::vec![ #( #methods, )* ],
					docs: #docs,
			}
	))
}

/// Generate the runtime metadata for the given traits.
pub fn generate_runtime_metadata(impls: &[ItemImpl], crate_: &TokenStream) -> Result<TokenStream> {
	if impls.is_empty() {
		return Ok(quote!())
	}

	// Get the name of the runtime for which the traits are implemented.
	let runtime_name = &impls
		.get(0)
		.expect("Traits should contain at least one implementation; qed")
		.self_ty;

	let mut metadata = Vec::new();

	for impl_ in impls {
		metadata.push(generate_trait_metadata(impl_, &crate_)?);
	}

	Ok(quote!(
		impl #runtime_name {
			pub fn runtime_metadata() -> #crate_::vec::Vec<#crate_::metadata::v15::TraitMetadata> {
				#crate_::vec![ #( #metadata, )* ]
			}
		}
	))
}