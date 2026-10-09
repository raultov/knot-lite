import type { User } from "../models/user";

/** In-memory user storage. */
export class UserRepository {
  private readonly users = new Map<number, User>();

  save(user: User): void {
    this.users.set(user.id, user);
  }

  findById(id: number): User | undefined {
    return this.users.get(id);
  }
}
