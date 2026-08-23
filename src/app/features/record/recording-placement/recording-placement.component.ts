import {
  ChangeDetectionStrategy,
  Component,
  computed,
  effect,
  inject,
  input,
  signal,
} from "@angular/core";

import { IpcService } from "../../../core/ipc.service";
import type { ContainerNode } from "../../../core/models";
import { ToastService } from "../../../services/toast.service";
import { WorkspaceService } from "../../workspace/workspace.service";

/** One selectable destination, flattened out of the container forest. */
interface Destination {
  id: string;
  name: string;
  emoji: string | null;
  /** 0 for a project, 1 for a folder inside one. Drives the indent only. */
  depth: number;
  level: "project" | "folder";
  /** Sealed and NOT unlocked for this session — cannot receive plaintext. */
  blocked: boolean;
}

/** localStorage key for the destination the user filed into last. */
const LAST_DESTINATION_KEY = "murmur.recording.lastDestination";

/**
 * Where the recording that just finished should live.
 *
 * A meeting is the one kind a user cannot create INTO a container: it creates
 * itself the moment recording stops, so the placement decision has to happen
 * after the fact or it never happens at all — which is how a vault ends up with
 * every meeting in one undifferentiated pile. The card therefore appears on
 * every resolved recording rather than only on unfiled ones: "you already filed
 * this one" is a judgement this component cannot make honestly (a meeting lands
 * in a DEFAULT destination, which is not the same as a chosen one), and asking
 * once is cheaper than a wrong guess in either direction.
 *
 * Filing routes through `move_note`, which is the ONLY mover that carries the
 * lock transitions: an open target moves the vault `.md`, a sealed target that
 * is session-unlocked seals the meeting on arrival so plaintext never lands
 * inside a sealed container, and a sealed target that is not unlocked is
 * refused outright. Those rows are therefore rendered disabled here rather than
 * offered and then rejected — the refusal is correct backend behaviour, but a
 * destination a click cannot reach should not look clickable.
 */
@Component({
  selector: "app-recording-placement",
  changeDetection: ChangeDetectionStrategy.OnPush,
  templateUrl: "./recording-placement.component.html",
  styleUrl: "./recording-placement.component.scss",
})
export class RecordingPlacementComponent {
  private readonly workspace = inject(WorkspaceService);
  private readonly ipc = inject(IpcService);
  private readonly toast = inject(ToastService);

  /** The meeting to file. Null while nothing has resolved yet. */
  readonly meetingId = input<string | null>(null);

  /** Where this recording ended up, once the user has picked. */
  private readonly _filedIn = signal<Destination | null>(null);
  readonly filedIn = this._filedIn.asReadonly();

  /** True while a move is in flight, so the list cannot be double-clicked. */
  private readonly _filing = signal(false);
  readonly filing = this._filing.asReadonly();

  /** Set when the user chooses to keep the default destination for now. */
  private readonly _dismissed = signal(false);
  readonly dismissed = this._dismissed.asReadonly();

  /** Free-text filter over destination names. */
  readonly query = signal("");

  /**
   * Reset the card for each new recording.
   *
   * Without this the "Filed in X" confirmation from the PREVIOUS recording would
   * still be showing when the next one resolves, and the user would believe a
   * meeting had been filed that never was.
   */
  private readonly _resetPerMeeting = effect(() => {
    this.meetingId();
    this._filedIn.set(null);
    this._dismissed.set(false);
    this.query.set("");
  });

  /** Load the forest once, so the card has somewhere to offer. */
  private readonly _load = effect(() => {
    if (this.meetingId() && this.workspace.forestEmpty()) {
      void this.workspace.reload();
    }
  });

  readonly destinations = computed<Destination[]>(() => {
    const flat: Destination[] = [];
    const push = (node: ContainerNode, depth: number): void => {
      flat.push({
        id: node.id,
        name: node.name,
        emoji: node.emoji,
        depth,
        level: node.level,
        blocked: node.locked && !node.unlocked,
      });
      for (const child of node.folders) {
        push(child, depth + 1);
      }
    };
    for (const project of this.workspace.forest()) {
      push(project, 0);
    }
    return flat;
  });

  /**
   * The filtered list, with the last-used destination hoisted to the top.
   *
   * Meetings cluster: the folder you filed the last one into is overwhelmingly
   * the folder you want for this one, and making the user re-find it in a long
   * list is what turns a two-second decision into one they stop making.
   */
  readonly visible = computed<Destination[]>(() => {
    const needle = this.query().trim().toLowerCase();
    const matches = needle
      ? this.destinations().filter((d) => d.name.toLowerCase().includes(needle))
      : this.destinations();
    const last = readLastDestination();
    if (!last) {
      return matches;
    }
    const hoisted = matches.find((d) => d.id === last && !d.blocked);
    return hoisted ? [hoisted, ...matches.filter((d) => d !== hoisted)] : matches;
  });

  readonly lastDestinationId = computed(() => readLastDestination());

  readonly empty = computed(() => this.destinations().length === 0);

  async file(destination: Destination): Promise<void> {
    const meetingId = this.meetingId();
    if (!meetingId || destination.blocked || this._filing()) {
      return;
    }
    this._filing.set(true);
    try {
      await this.workspace.moveItem("meeting", meetingId, destination.id);
      writeLastDestination(destination.id);
      this._filedIn.set(destination);
    } catch (error) {
      this.toast.push(
        `Could not file this recording: ${messageOf(error)}`,
        "danger",
      );
    } finally {
      this._filing.set(false);
    }
  }

  /** Re-open the list after a successful file, so a mis-click is recoverable. */
  change(): void {
    this._filedIn.set(null);
  }

  dismiss(): void {
    this._dismissed.set(true);
  }
}

function readLastDestination(): string | null {
  try {
    return localStorage.getItem(LAST_DESTINATION_KEY);
  } catch {
    // A private window, cleared site data, or a browser blocking storage. The
    // card works without the hoist; it must not fail to render over it.
    return null;
  }
}

function writeLastDestination(id: string): void {
  try {
    localStorage.setItem(LAST_DESTINATION_KEY, id);
  } catch {
    // Non-fatal — see readLastDestination.
  }
}

function messageOf(error: unknown): string {
  if (typeof error === "string") {
    return error;
  }
  if (error && typeof error === "object" && "message" in error) {
    return String((error as { message: unknown }).message);
  }
  return String(error);
}
