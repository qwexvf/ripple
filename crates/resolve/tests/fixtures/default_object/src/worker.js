export default {
  async fetch(request, env) {
    return handle(request);
  },
};
function handle(r) { return r; }
