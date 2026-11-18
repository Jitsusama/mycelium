# The `mycelium` command, built from the crate with its tests run as it
# builds.
{
  lib,
  stdenv,
  rustPlatform,
  git,
}:
rustPlatform.buildRustPackage {
  pname = "mycelium";
  version = "0.1.0";
  src = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.unions [
      ../Cargo.toml
      ../Cargo.lock
      ../src
      # Cargo.toml names the benches, so they must be there to build.
      ../benches
      ../tests
    ];
  };
  cargoLock.lockFile = ../Cargo.lock;
  # The tests make a git clone, as a machine does of one another holds.
  nativeCheckInputs = [ git ];
  # Nix's sandbox on a Mac lets a build start no FSEvents stream, which
  # notify watches files through there, and bind no listener, even on the
  # loopback. So these tests, which hear a file change or bind a listener
  # of their own, fail in a Mac's Nix build alone: they still run under
  # cargo test on a Mac, and in the Linux build, where notify uses
  # inotify. `--exact` makes each skip match a whole name, so a test added
  # later is never skipped unseen.
  checkFlags = lib.optionals stdenv.hostPlatform.isDarwin (
    [ "--exact" ]
    ++ map (test: "--skip=${test}") (
      # A listener of its own.
      [
        "builders::reach::tests::a_builder_answers_only_when_it_greets_as_ssh_does"
        "builders::reach::tests::a_builder_saying_nothing_in_time_hasnt_answered"
        "builders::reach::tests::a_builder_straight_there_is_dialled_where_ssh_says_it_is"
        "builders::tests::a_builder_dialled_lately_isnt_dialled_again_though_a_read_missed_it"
        "builders::tests::a_builder_once_lent_is_never_dialled_though_its_lent_file_goes"
        "builders::tests::builders_are_heard_as_they_stand_and_a_lent_one_is_never_dialled"
        "reach::tests::a_network_is_reached_while_its_probe_answers"
      ]
      # A file heard as it changes.
      ++ [
        "forage::tests::a_document_written_is_told_and_read"
        "hold::holder::tests::a_command_named_for_a_running_program_wins_over_the_one_it_names_as_it_parks"
        "hold::holder::tests::a_parked_program_starts_again_as_a_steer_that_wakes_it_lands_in_its_inbox"
        "hold::holder::tests::a_parked_program_starts_again_as_its_window_comes_in_front"
        "hold::holder::tests::a_parked_program_starts_again_on_its_terminal_when_a_key_is_typed"
        "hold::holder::tests::a_parked_program_starts_again_when_its_wake_file_appears"
        "hold::holder::tests::a_parked_program_starts_again_with_the_command_it_named"
        "hold::holder::tests::a_parked_program_that_cant_start_again_stays_parked_and_says_why"
        "hold::holder::tests::a_window_is_listed_only_while_it_is_joined"
        "hold::holder::tests::a_woken_program_has_the_persons_environment_as_it_is_then"
        "hold::holder::tests::every_start_of_a_parked_program_runs_inside_the_words_it_was_held_through"
        "hold::holder::tests::naming_a_parked_programs_next_command_wakes_it_with_that_command"
        "hold::steering::tests::a_steer_landing_in_an_inbox_made_later_is_heard_once_it_is_watched"
        "hold::steering::tests::an_inbox_removed_and_made_again_is_watched_again"
        "hold::steering::tests::arming_sees_a_steer_that_landed_before_the_watch_began"
        "hold::steering::tests::what_is_heard_in_one_inbox_is_judged_by_that_inbox_alone"
        "node::tests::a_carry_another_machine_told_of_is_shown_as_its_session_is_seen_here"
        "node::tests::a_carry_back_over_a_link_is_kept_where_its_quest_runs"
        "node::tests::a_carry_ordered_is_shown_standing_as_its_session_is_seen"
        "node::tests::a_carry_the_persons_own_login_asks_is_kept_as_theirs"
        "node::tests::a_cell_here_is_dormant_only_once_its_park_is_marked"
        "node::tests::a_cell_working_on_a_quest_is_served_its_forage_as_it_changes"
        "node::tests::a_chain_forged_in_the_quests_record_orders_no_carry"
        "node::tests::a_fenced_agent_is_never_offered_looser_ground"
        "node::tests::a_fenced_session_runs_nothing_on_looser_ground"
        "node::tests::a_file_the_node_serves_is_a_kind_its_clients_watch"
        "node::tests::a_node_says_its_builders_apart_from_its_machines"
        "node::tests::a_node_serves_its_cells_murmurs_newest_first"
        "node::tests::a_persons_carry_keeps_nothing_where_no_quests_record_holds_its_session"
        "node::tests::a_presence_borrowing_the_orchestrators_session_orders_no_carry"
        "node::tests::a_proved_session_whose_address_is_in_small_letters_is_told_it_in_capitals"
        "node::tests::a_proved_session_whose_address_reads_as_a_word_is_unknown"
        "node::tests::a_provider_is_never_told_a_session_only_claimed_above_the_asker"
        "node::tests::a_provider_is_told_the_address_of_the_session_proved_to_ask"
        "node::tests::a_quest_no_orchestrator_told_the_node_of_orders_no_carry"
        "node::tests::a_quest_the_quests_record_says_ended_is_ended_by_who_orchestrated_it"
        "node::tests::a_receptor_reading_is_sent_to_a_watch_of_cells_at_once"
        "node::tests::a_receptors_climb_the_table_names_is_said_as_its_cell_and_a_planted_one_isnt"
        "node::tests::a_record_of_ended_quests_naming_another_orchestrator_proves_nothing"
        "node::tests::a_session_here_that_ended_on_an_orchestrated_quest_is_told_while_the_quest_is_active"
        "node::tests::a_session_whose_log_continues_an_orchestrator_takes_on_its_quests"
        "node::tests::a_sibling_heading_its_orchestrators_quest_ends_nothing_of_one_that_ended"
        "node::tests::a_watch_of_sessions_says_how_each_stands_and_hears_a_park_marked"
        "node::tests::an_agents_start_is_never_placed_on_looser_ground"
        "node::tests::an_orchestrator_ends_a_session_it_orchestrates_held_here"
        "node::tests::an_orchestrator_ends_a_session_of_a_quest_that_ended"
        "node::tests::an_orchestrator_started_afresh_claims_its_quests_again"
        "node::tests::an_ordered_carry_counts_a_fenced_command_below_a_session_claiming_none"
        "node::tests::an_ordered_carry_never_lands_on_looser_ground"
        "node::tests::an_ordered_carry_of_a_session_whose_owner_isnt_proved_is_refused"
        "node::tests::an_ordered_carry_remembers_the_strictest_fence_its_session_was_seen_in"
        "node::tests::an_ordered_carry_takes_the_stricter_of_the_fence_claimed_and_shown"
        "node::tests::only_an_orchestrator_by_the_quests_record_orders_a_carry"
        "node::tests::only_the_persons_own_login_frees_a_quest"
        "node::tests::only_the_session_holding_an_orchestration_hands_it_on"
        "node::tests::the_source_a_receptor_senses_is_a_network_the_node_serves"
        "served::tests::a_file_written_whole_changes_only_the_records_that_changed_in_it"
        "served::tests::a_record_whose_act_is_done_is_shown_no_more"
        "served::tests::a_served_file_is_read_again_as_it_changes_and_its_records_go_with_it"
        "served::tests::a_talk_is_heard_as_its_session_writes_it_in_an_inbox_made_since"
        "served::tests::a_talk_is_heard_once_its_inboxes_are_made_after_the_node_started"
        "served::tests::an_act_done_since_it_was_shown_is_seen_gone_with_its_file_as_it_was"
        "share::exchange::tests::a_file_found_as_it_was_is_not_read_again"
        "share::exchange::tests::a_scratch_taken_away_whole_is_no_word_on_the_other_machines"
        "share::exchange::tests::only_the_scratch_folders_named_cross_and_the_far_ends_own_moves_aside"
        "share::watch::tests::a_change_is_matched_to_its_folder_through_a_linked_scratch"
        "share::watch::tests::a_change_to_what_never_crosses_needs_no_walk"
        "share::watch::tests::a_folder_is_walked_when_named_and_then_only_once_it_changes"
        "share::watch::tests::a_folder_not_there_yet_is_walked_until_it_can_be_watched"
        "share::watch::tests::a_record_is_walked_when_first_looked_at_and_then_only_once_it_changes"
        "store::folders::tests::a_machine_saying_itself_again_is_told_by_its_name"
        "store::folders::tests::a_message_written_through_a_linked_path_is_told"
        "store::folders::tests::a_process_said_to_have_ended_ends_its_session_until_it_is_present"
        "store::folders::tests::reading_a_message_is_no_change"
        "store::folders::tests::the_carries_a_machine_told_of_are_kept_in_place_of_the_last_until_none"
        "watching::tests::a_watch_hears_a_file_written_and_never_one_read"
      ]
    )
  );
  meta = {
    description = "A nimble core for agent networks";
    license = lib.licenses.mit;
    mainProgram = "mycelium";
  };
}
