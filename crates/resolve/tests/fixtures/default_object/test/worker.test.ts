import worker from '../src/worker.js';
const fetchIt = (req: Request) => worker.fetch(req, {} as never);
