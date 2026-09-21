from orchestratelang import landline
class P(landline.Serverlet):
    def ping(self) -> int: return 1
landline.serve(P)
