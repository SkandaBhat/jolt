use crate::{jolt::{self, vm::JoltCommitments}, poly::{dense_mlpoly::DensePolynomial, hyrax::{HyraxCommitment, HyraxGenerators}}, r1cs::r1cs_shape::R1CSShape, utils::thread::drop_in_background_thread};

use super::{constraints::R1CSBuilder, spartan::{SpartanError, UniformShapeBuilder, UniformSpartanKey, UniformSpartanProof}}; 

use ark_ec::CurveGroup;
use common::{constants::{NUM_R1CS_POLYS, RAM_START_ADDRESS}, field_conversion::{ark_to_spartan_unsafe, spartan_to_ark_unsafe}, path::JoltPaths};
use itertools::Itertools;
// use spartan2::{
//   errors::SpartanError, provider::{
//       bn256_grumpkin::bn256::{self, Point as SpartanG1, Scalar as Spartan2Fr},
//       hyrax_pc::{HyraxCommitment as SpartanHyraxCommitment, HyraxCommitmentKey, HyraxEvaluationEngine as SpartanHyraxEE},
//   }, r1cs::R1CSShape, spartan::upsnark::R1CSSNARK, traits::{
//     commitment::CommitmentEngineTrait, upsnark::PrecommittedSNARKTrait, Group
//   }, VerifierKey, SNARK
// };
use bellpepper_core::{
  Circuit, ConstraintSystem, SynthesisError
};
// use ff::PrimeField;
use ark_ff::PrimeField;
use merlin::Transcript;
use rayon::prelude::*;

/// Reorder and drop first element [[a1, b1, c1], [a2, b2, c2]] => [[a2], [b2], [c2]]
#[tracing::instrument(skip_all)]
fn reassemble_segments<F: PrimeField>(jolt_witnesses: Vec<Vec<F>>) -> Vec<Vec<F>> {
  let trace_len = jolt_witnesses.len();
  let num_variables = jolt_witnesses[0].len();
  let mut result: Vec<Vec<F>> = vec![vec![F::ZERO; trace_len]; num_variables - 1]; // ignore 1 

  result.par_iter_mut().enumerate().for_each(|(variable_idx, variable_segment)| {
    for step in 0..trace_len {
      variable_segment[step] = jolt_witnesses[step][variable_idx]; // NOTE: 1 is at the end!
    }
  });

  result 
}

/// Reorder and drop first element [[a1, b1, c1], [a2, b2, c2]] => [[a2], [b2], [c2]]
#[tracing::instrument(skip_all, name = "reassemble_segments_partial")]
fn reassemble_segments_partial<F: PrimeField>(jolt_witnesses: Vec<Vec<F>>, num_front: usize, num_back: usize) -> (Vec<Vec<F>>, Vec<Vec<F>>) {
  let trace_len = jolt_witnesses.len();
  let total_length = jolt_witnesses[0].len();
  let mut front_result: Vec<Vec<F>> = vec![vec![F::ZERO; trace_len]; num_front]; 
  let mut back_result: Vec<Vec<F>> = vec![vec![F::ZERO; trace_len]; num_back]; 

  // [1 || output_state] starts at the beginning
  front_result.par_iter_mut().enumerate().for_each(|(variable_idx, variable_segment)| {
    for step in 0..trace_len {
      variable_segment[step] = jolt_witnesses[step][variable_idx+1]; // NOTE: 1 is at the beginning!
    }
  });

  // [.. || aux] is the end
  back_result.par_iter_mut().enumerate().for_each(|(variable_idx, variable_segment)| {
    for step in 0..trace_len {
      variable_segment[step] = jolt_witnesses[step][(total_length-num_back) + variable_idx]; 
    }
  });

  drop_in_background_thread(jolt_witnesses);

  (front_result, back_result)
}

#[derive(Clone, Debug, Default)]
pub struct JoltCircuit<F: PrimeField> {
  num_steps: usize,
  inputs: R1CSInputs<F>,
}

impl<F: PrimeField> JoltCircuit<F> {
  pub fn new_from_inputs(num_steps: usize, inputs: R1CSInputs<F>) -> Self {
    JoltCircuit{
      num_steps: num_steps,
      inputs: inputs,
    }
  }

  #[tracing::instrument(name = "synthesize_state_aux_segments", skip_all)]
  pub fn synthesize_state_aux_segments(&self, num_state: usize, num_aux: usize) -> (Vec<Vec<F>>, Vec<Vec<F>>) {
    let jolt_witnesses = self.synthesize_witnesses();
    // TODO(sragss / arasuarun): Synthsize witnesses should just return (io, aux)
    reassemble_segments_partial(jolt_witnesses, num_state, num_aux)
  }

  #[tracing::instrument(name = "JoltCircuit::synthesize_witnesses", skip_all)]
  fn synthesize_witnesses(&self) -> Vec<Vec<F>> {
    let mut step_z = self.inputs.clone_to_stepwise();

    // Compute the aux
    let span = tracing::span!(tracing::Level::INFO, "calc_aux");
    let _guard = span.enter();
    step_z.par_iter_mut().enumerate().for_each(|(i, step)| {
      R1CSBuilder::calculate_aux(step);
    });

    step_z
  }
}


// #[tracing::instrument(name = "get_w_segments", skip_all)]
// pub fn get_w_segments<G: Group<Scalar = F>, S: PrecommittedSNARKTrait<G>, F: PrimeField<Repr = [u8; 32]>>(jolt_circuit: &JoltCircuit<F>) -> Result<(Vec<Vec<F>>), SynthesisError> {
//   let jolt_witnesses = jolt_circuit.get_witnesses_by_step()?;
//   Ok(get_segments(jolt_witnesses))
// }

// pub fn precommit_with_ck<G: Group<Scalar = F>, S: PrecommittedSNARKTrait<G>, F: PrimeField<Repr = [u8; 32]>>(ck: &CommitmentKey<G>, w_segments: Vec<Vec<F>>) -> Result<(Vec<<G::CE as CommitmentEngineTrait<G>>::Commitment>), SynthesisError> {
//   let N_SEGMENTS = w_segments.len();

//   // for each segment, commit to it using CE::<G>::commit(ck, &self.W) 
//   let commitments: Vec<<G::CE as CommitmentEngineTrait<G>>::Commitment> = (0..N_SEGMENTS)
//     .into_par_iter()
//     .map(|i| {
//       G::CE::commit(&ck, &w_segments[i]) 
//     }).collect();

//   Ok(commitments)
// }


#[derive(Clone, Debug, Default)]
pub struct R1CSInputs<F: PrimeField> {
    bytecode_a: Vec<F>,
    bytecode_v: Vec<F>,
    memreg_a_rw: Vec<F>,
    memreg_v_reads: Vec<F>,
    memreg_v_writes: Vec<F>,
    chunks_x: Vec<F>,
    chunks_y: Vec<F>,
    chunks_query: Vec<F>,
    lookup_outputs: Vec<F>,
    circuit_flags_bits: Vec<F>,
}

impl<F: PrimeField> R1CSInputs<F> {
  #[tracing::instrument(skip_all, name = "R1CSInputs::new")]
  pub fn new(
    bytecode_a: Vec<F>,
    bytecode_v: Vec<F>,
    memreg_a_rw: Vec<F>,
    memreg_v_reads: Vec<F>,
    memreg_v_writes: Vec<F>,
    chunks_x: Vec<F>,
    chunks_y: Vec<F>,
    chunks_query: Vec<F>,
    lookup_outputs: Vec<F>,
    circuit_flags_bits: Vec<F>
  ) -> Self {

    Self {
      bytecode_a,
      bytecode_v,
      memreg_a_rw,
      memreg_v_reads,
      memreg_v_writes,
      chunks_x,
      chunks_y,
      chunks_query,
      lookup_outputs,
      circuit_flags_bits,
    }
  }

  #[tracing::instrument(skip_all, name = "R1CSInputs::clone_to_stepwise")]
  pub fn clone_to_stepwise(&self) -> Vec<Vec<F>> {
    const PREFIX_VARS_PER_STEP: usize = 5;

    // AUX_VARS_PER_STEP has to be greater than the number of additional vars pushed by the constraint system
    const AUX_VARS_PER_STEP: usize = 20; 
    let num_inputs_per_step = self.num_vars_per_step() + PREFIX_VARS_PER_STEP;

    let stepwise = (0..self.trace_len()).into_par_iter().map(|step_index| {
      let mut step: Vec<F> = Vec::with_capacity(num_inputs_per_step + AUX_VARS_PER_STEP);
      let program_counter = if step_index > 0 && self.bytecode_a[step_index] == F::ZERO {
        F::ZERO
      } else {
        self.bytecode_a[step_index] * F::from(4u64) + F::from(RAM_START_ADDRESS)
      };
      // TODO(sragss / arasu arun): This indexing strategy is stolen from old -- but self.trace_len here is self.bytecode_a.len() -- not sure why we're using that to split inputs.

      // 1 is constant, 0s in slots 1, 2 are filled by aux computation
      step.extend([F::from(1u64), F::from(0u64), F::from(0u64), F::from(step_index as u64), program_counter]);
      let bytecode_a_num_vals = self.bytecode_a.len() / self.trace_len();
      for var_index in 0..bytecode_a_num_vals {
        step.push(self.bytecode_a[var_index * self.trace_len() + step_index]);
      }
      let bytecode_v_num_vals = self.bytecode_v.len() / self.trace_len();
      for var_index in 0..bytecode_v_num_vals {
        step.push(self.bytecode_v[var_index * self.trace_len() + step_index]);
      }
      let memreg_a_rw_num_vals = self.memreg_a_rw.len() / self.trace_len();
      for var_index in 0..memreg_a_rw_num_vals {
        step.push(self.memreg_a_rw[var_index * self.trace_len() + step_index]);
      }
      let memreg_v_reads_num_vals = self.memreg_v_reads.len() / self.trace_len();
      for var_index in 0..memreg_v_reads_num_vals {
        step.push(self.memreg_v_reads[var_index * self.trace_len() + step_index]);
      }
      let memreg_v_writes_num_vals = self.memreg_v_writes.len() / self.trace_len();
      for var_index in 0..memreg_v_writes_num_vals {
        step.push(self.memreg_v_writes[var_index * self.trace_len() + step_index]);
      }
      let chunks_x_num_vals = self.chunks_x.len() / self.trace_len();
      for var_index in 0..chunks_x_num_vals {
        step.push(self.chunks_x[var_index * self.trace_len() + step_index]);
      }
      let chunks_y_num_vals = self.chunks_y.len() / self.trace_len();
      for var_index in 0..chunks_y_num_vals {
        step.push(self.chunks_y[var_index * self.trace_len() + step_index]);
      }
      let chunks_query_num_vals = self.chunks_query.len() / self.trace_len();
      for var_index in 0..chunks_query_num_vals {
        step.push(self.chunks_query[var_index * self.trace_len() + step_index]);
      }
      let lookup_outputs_num_vals = self.lookup_outputs.len() / self.trace_len();
      for var_index in 0..lookup_outputs_num_vals {
        step.push(self.lookup_outputs[var_index * self.trace_len() + step_index]);
      }
      let circuit_flags_bits_num_vals = self.circuit_flags_bits.len() / self.trace_len();
      for var_index in 0..circuit_flags_bits_num_vals {
        step.push(self.circuit_flags_bits[var_index * self.trace_len() + step_index]);
      }

      assert_eq!(num_inputs_per_step, step.len());

      step
    }).collect();

    stepwise
  }


  pub fn trace_len(&self) -> usize {
      self.bytecode_a.len()
  }

  pub fn num_vars_per_step(&self) -> usize {
    let trace_len = self.trace_len();
    self.bytecode_a.len() / trace_len
      + self.bytecode_v.len() / trace_len
      + self.memreg_a_rw.len() / trace_len
      + self.memreg_v_reads.len() / trace_len
      + self.memreg_v_writes.len() / trace_len
      + self.chunks_x.len() / trace_len
      + self.chunks_y.len() / trace_len
      + self.chunks_query.len() / trace_len
      + self.lookup_outputs.len() / trace_len
      + self.circuit_flags_bits.len() / trace_len
  }

  #[tracing::instrument(skip_all, name = "R1CSInputs::trace_len_chunks")]
  pub fn trace_len_chunks(&self, padded_trace_len: usize) -> Vec<Vec<F>> {
    // TODO(sragss / arasuarun): Explain why non-trace-len relevant stuff (ex: bytecode) gets chunked to trace_len
    let mut chunks: Vec<Vec<F>> = Vec::new();
    chunks.par_extend(self.bytecode_a.par_chunks(padded_trace_len).map(|chunk| chunk.to_vec()));
    chunks.par_extend(self.bytecode_v.par_chunks(padded_trace_len).map(|chunk| chunk.to_vec()));
    chunks.par_extend(self.memreg_a_rw.par_chunks(padded_trace_len).map(|chunk| chunk.to_vec()));
    chunks.par_extend(self.memreg_v_reads.par_chunks(padded_trace_len).map(|chunk| chunk.to_vec()));
    chunks.par_extend(self.memreg_v_writes.par_chunks(padded_trace_len).map(|chunk| chunk.to_vec()));
    chunks.par_extend(self.chunks_x.par_chunks(padded_trace_len).map(|chunk| chunk.to_vec()));
    chunks.par_extend(self.chunks_y.par_chunks(padded_trace_len).map(|chunk| chunk.to_vec()));
    chunks.par_extend(self.chunks_query.par_chunks(padded_trace_len).map(|chunk| chunk.to_vec()));
    chunks.par_extend(self.lookup_outputs.par_chunks(padded_trace_len).map(|chunk| chunk.to_vec()));
    chunks.par_extend(self.circuit_flags_bits.par_chunks(padded_trace_len).map(|chunk| chunk.to_vec()));
    chunks
  }
}

pub struct R1CSProof<F: PrimeField, G: CurveGroup<ScalarField = F>>  {
  pub key: UniformSpartanKey<F>,
  proof: UniformSpartanProof<F, G>,
  pub commitments: R1CSCommitments<F, G>
}

pub struct R1CSCommitments<F: PrimeField, G: CurveGroup<ScalarField = F>> {
  witness_segment_commitments: Vec<HyraxCommitment<NUM_R1CS_POLYS, G>>,
  generators: HyraxGenerators<NUM_R1CS_POLYS, G>
}

impl<F: PrimeField, G: CurveGroup<ScalarField = F>> R1CSProof<F, G> {
  #[tracing::instrument(skip_all, name = "R1CSProof::prove")]
  pub fn prove<ArkF: ark_ff::PrimeField> (
      _W: usize, 
      _C: usize, 
      padded_trace_len: usize, 
      inputs: R1CSInputs<F>,
      generators: HyraxGenerators<1, G>,
      prior_commitments: &Vec<HyraxCommitment<1, G>>,
      transcript: &mut Transcript
  ) -> Result<Self, SpartanError> {
      let num_steps = padded_trace_len;

      let span = tracing::span!(tracing::Level::TRACE, "JoltCircuit::new_from_inputs");
      let _enter = span.enter();
      // TODO(sragss / arasuarun): After Spartan is merged we don't need to clone these inputs anymore
      let jolt_circuit = JoltCircuit::<F>::new_from_inputs(num_steps, inputs.clone());
      drop(_enter);
      
      let span = tracing::span!(tracing::Level::TRACE, "shape_stuff");
      let _enter = span.enter();
      let mut jolt_shape = R1CSBuilder::default(); 
      R1CSBuilder::get_matrices(&mut jolt_shape); 
      let key = UniformSpartanProof::<F,G>::setup_precommitted(&jolt_shape, padded_trace_len)?;
      // let constraints_F = jolt_shape.convert_to_field(); 
      // let shape_single = R1CSShape::<F> {
      //     A: constraints_F.0,
      //     B: constraints_F.1,
      //     C: constraints_F.2,
      //     num_cons: jolt_shape.num_constraints,
      //     num_vars: jolt_shape.num_aux, // shouldn't include 1 or IO 
      //     num_io: jolt_shape.num_inputs,
      // };
      drop(_enter);
      drop(span);

      let (io_segments, aux_segments) = jolt_circuit.synthesize_state_aux_segments(4, jolt_shape.num_internal);

      let cloning_stuff_span = tracing::span!(tracing::Level::TRACE, "cloning_stuff");
      let _enter = cloning_stuff_span.enter();

      let inputs_segments = inputs.trace_len_chunks(padded_trace_len);

      let mut w_segments: Vec<Vec<F>> = Vec::with_capacity(io_segments.len() + inputs_segments.len() + aux_segments.len());
      // TODO(sragss / arasuarun): rm clones in favor of references
      w_segments.par_extend(io_segments.par_iter().cloned());
      w_segments.par_extend(inputs_segments.into_par_iter());
      w_segments.par_extend(aux_segments.par_iter().cloned());

      drop(_enter);
      drop(cloning_stuff_span);

      // Commit to segments
      let commit_segments = |segments: Vec<Vec<F>>| -> Vec<HyraxCommitment<1, G>> {
        let span = tracing::span!(tracing::Level::TRACE, "commit_segments");
        let _g = span.enter();
        segments.into_par_iter().map(|segment| {
          HyraxCommitment::commit(&DensePolynomial::new(segment), &generators)
        }).collect()
      };

      let io_comms: Vec<HyraxCommitment<1, G>> = commit_segments(io_segments);
      let input_comms = prior_commitments; 
      let aux_comms = commit_segments(aux_segments);

      println!("io_comms.len(): {}", io_comms.len());
      println!("input_comms.len(): {}", input_comms.len());
      println!("aux_comms.len(): {}", aux_comms.len());

      // TODO(sragss): Likely want to append this commitment to jolt_commitments
      let witness_segment_commitments = io_comms.into_iter()
        .chain(input_comms.iter().map(|comm| HyraxCommitment::from(comm.clone())))
        .chain(aux_comms.into_iter())
        .collect::<Vec<_>>();

      let r1cs_commitments = R1CSCommitments::<F,G> {
        witness_segment_commitments,
        generators
      };

      // TODO(sragss / arasuarun): Wire up remainder here.
      let proof = UniformSpartanProof::prove_precommitted(&key, w_segments, &r1cs_commitments.witness_segment_commitments, transcript).expect("UniformSpartanProof failed");
      Ok(R1CSProof::<F, G> {
        proof,
        key,
        commitments: r1cs_commitments
      })
  }

  pub fn verify(&self, transcript: &mut Transcript) -> Result<(), SpartanError> {
    self.proof.verify_precommitted(&self.key, &[], &self.commitments.generators, transcript)
  }
}

impl<F: PrimeField> UniformShapeBuilder<F> for R1CSBuilder {
  fn single_step_shape(&self) -> R1CSShape<F> {
    let mut jolt_shape = R1CSBuilder::default(); 
    R1CSBuilder::get_matrices(&mut jolt_shape); 
    let constraints_F = jolt_shape.convert_to_field(); 
    let shape_single = R1CSShape::<F> {
        A: constraints_F.0,
        B: constraints_F.1,
        C: constraints_F.2,
        num_cons: jolt_shape.num_constraints,
        num_vars: jolt_shape.num_aux, // shouldn't include 1 or IO 
        num_io: jolt_shape.num_inputs,
    };

    shape_single.pad_vars()
  }
}
