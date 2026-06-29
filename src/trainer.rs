//! The training loop: present a spot, take the user's action, score it against
//! the solver's strategy, report EV loss, repeat.

/// Entry point for `poker-trainer drill`.
pub fn run_drill() {
    println!("poker-trainer — drill mode (scaffold)");
    println!();
    println!("Phase 1 fills in the loop below:");
    println!("  1. deal a spot (positions, pot type, depth, board)");
    println!("  2. read the GTO strategy via a SolutionProvider");
    println!("  3. take your action (+ an RNG roll for mixed spots)");
    println!("  4. score: is the action in-strategy? how much EV is lost?");
    println!("  5. show the full optimal frequency mix, then repeat");

    // Sketch:
    //   let provider = FileSolutionProvider::new("sims_cache");
    //   loop {
    //       let spot = next_spot();
    //       let action = prompt_user(&spot);
    //       let strat = match provider.strategy(&spot.board, spot.hero) {
    //           Some(s) => s,
    //           None => continue,
    //       };
    //       let loss = score(&action, &strat); // EV loss in bb + in-strategy flag
    //       report(loss, &strat);
    //   }
}
