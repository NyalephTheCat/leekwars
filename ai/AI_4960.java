import leekscript.runner.*;
import leekscript.runner.values.*;
import leekscript.runner.classes.*;
import leekscript.common.*;

public class AI_4960 extends AI {
public AI_4960() throws LeekRunException {
super(1, 4);
}
public void staticInit() throws LeekRunException {
}
public Object runIA(Session session) throws LeekRunException {
session.rebindAll(this);
var u_m = session.getVariable("m");
return ops(putv4(u_m.get(), "a", 1l, null), 1);
}
protected String getAIString() { return "<snippet 4960>";}
protected String[] getErrorFiles() { return new String[] {"<snippet 4960>", };}

protected int[] getErrorFilesID() { return new int[] {4960, };}

}
