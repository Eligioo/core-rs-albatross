use algebra::curves::bls12_377::Bls12_377Parameters;
use algebra::fields::sw6::Fr as SW6Fr;
use r1cs_core::SynthesisError;
use r1cs_std::groups::curves::short_weierstrass::bls12::G2Gadget;

use r1cs_std::bits::boolean::Boolean;
use r1cs_std::eq::ConditionalEqGadget;
use r1cs_std::groups::curves::short_weierstrass::bls12::G1Gadget;
use r1cs_std::pairing::bls12_377::PairingGadget;
use r1cs_std::pairing::PairingGadget as PG;

pub struct CheckSigGadget {}

impl CheckSigGadget {
    pub fn conditional_check_signature<CS: r1cs_core::ConstraintSystem<SW6Fr>>(
        mut cs: CS,
        public_key: &G2Gadget<Bls12_377Parameters>,
        generator: &G2Gadget<Bls12_377Parameters>,
        signature: &G1Gadget<Bls12_377Parameters>,
        hash_point: &G1Gadget<Bls12_377Parameters>,
        condition: &Boolean,
    ) -> Result<(), SynthesisError> {
        let sig_p_var = PairingGadget::prepare_g1(&mut cs.ns(|| "sig_p"), &signature)?;
        let hash_p_var = PairingGadget::prepare_g1(&mut cs.ns(|| "hash_p"), &hash_point)?;

        let generator_p_var = PairingGadget::prepare_g2(&mut cs.ns(|| "generator_p"), &generator)?;
        let pub_key_p_var = PairingGadget::prepare_g2(&mut cs.ns(|| "pub_key_p"), &public_key)?;

        let pairing1_var = PairingGadget::pairing(
            &mut cs.ns(|| "sig pairing"),
            sig_p_var.clone(),
            generator_p_var.clone(),
        )?;
        let pairing2_var =
            PairingGadget::pairing(&mut cs.ns(|| "pub pairing"), hash_p_var, pub_key_p_var)?;

        pairing1_var.conditional_enforce_equal(
            &mut cs.ns(|| "pairing equality"),
            &pairing2_var,
            condition,
        )?;
        Ok(())
    }
}