/**
 * What this connection has sent and is still waiting to see come back.
 *
 * The painter names each operation it draws ahead of the server, and the echo
 * has to be handed back under that name for the painter to recognise it as its
 * own. The wire bytes are all the echo carries, and they are not a name: an
 * undo boundary is the same two bytes every time, and so is any stroke drawn
 * twice. What makes an echo findable is order -- the server sequences one
 * connection's messages in the order that connection sent them.
 *
 * It does not promise to sequence all of them. A room whose history is full,
 * or a Redis error, drops a message without a word, and the echo never comes.
 * A queue per distinct wire payload could not notice: the dropped boundary's
 * name stayed at the head of the boundary queue and every later boundary was
 * answered with the one before it, which the painter took for a divergence and
 * rebuilt its canvas over, stroke after stroke, until the next reconnect. One
 * queue in sending order can: an echo that matches an entry further down means
 * every entry ahead of it was never sequenced and never will be. The painter
 * already drops those from its fork when an echo passes them by.
 */
export class PendingEchoes {
  private sent: { wireId: string; id: string }[] = [];

  record(wireId: string, id: string): void {
    this.sent.push({ wireId, id });
  }

  /**
   * The painter's name for an echo of our own, or the wire id when it is not
   * one we are waiting for -- the same person drawing in another tab arrives
   * under the same session id, and those operations were never in our fork.
   */
  claim(wireId: string): string {
    const index = this.sent.findIndex((entry) => entry.wireId === wireId);
    if (index === -1) return wireId;
    const { id } = this.sent[index];
    this.sent.splice(0, index + 1);
    return id;
  }

  clear(): void {
    this.sent.length = 0;
  }

  get size(): number {
    return this.sent.length;
  }
}
